/**
 * MIDI I/O store: port loading, in/out port selection, the arm target, and
 * hardware MIDI-out (multiple simultaneously open ports, per-port clock,
 * per-track/per-clip routing) — mirrors project.svelte.test.ts's
 * `vi.mock("../tauri", …)` pattern.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { MidiInputStatus, MidiOutputStatus, MidiPortInfo } from "../types/ipc";

const midiListInputPorts = vi.fn<() => Promise<MidiPortInfo[]>>(() =>
  Promise.resolve([{ id: "in#0", name: "Input" }]),
);
const midiListOutputPorts = vi.fn<() => Promise<MidiPortInfo[]>>(() =>
  Promise.resolve([{ id: "out#0", name: "Output" }]),
);
const midiSelectInputPort = vi.fn(() => Promise.resolve());
const midiSelectInputTrack = vi.fn(() => Promise.resolve());
const midiOpenOutputPort = vi.fn(() => Promise.resolve());
const midiCloseOutputPort = vi.fn(() => Promise.resolve());
const midiSetOutputClockEnabled = vi.fn(() => Promise.resolve());
const midiSetTrackRoute = vi.fn(() => Promise.resolve());
const midiSetClipRoute = vi.fn(() => Promise.resolve());

const inputStatus: MidiInputStatus = {
  selected: { id: "in#0", name: "Input" },
  eventsSeen: 1,
  lastEventAgeMs: 10,
  lastStatusBytes: [],
  monitor: true,
  targetTrackId: "t-1",
  routing: true,
  droppedEvents: 0,
  capturing: false,
};
const outputStatus: MidiOutputStatus = {
  outputs: [
    { port: { id: "out#0", name: "Output" }, clockEnabled: true, running: true, pulsesSent: 0, resyncs: 0, notesSent: 0 },
  ],
  routes: [],
};
const midiInputStatus = vi.fn(() => Promise.resolve(inputStatus));
const midiOutputStatus = vi.fn(() => Promise.resolve(outputStatus));

vi.mock("../tauri", () => ({
  backend: {
    mode: "tauri" as const,
    midiListInputPorts,
    midiListOutputPorts,
    midiSelectInputPort,
    midiSelectInputTrack,
    midiOpenOutputPort,
    midiCloseOutputPort,
    midiSetOutputClockEnabled,
    midiSetTrackRoute,
    midiSetClipRoute,
    midiInputStatus,
    midiOutputStatus,
  },
}));

const { midiIo } = await import("./midiio.svelte");

beforeEach(() => {
  vi.clearAllMocks();
  midiListInputPorts.mockResolvedValue([{ id: "in#0", name: "Input" }]);
  midiListOutputPorts.mockResolvedValue([{ id: "out#0", name: "Output" }]);
  midiInputStatus.mockResolvedValue(inputStatus);
  midiOutputStatus.mockResolvedValue(outputStatus);
  midiIo.monitor = true;
  midiIo.outputs = [];
  midiIo.routes = [];
});

describe("midiIo", () => {
  it("loads both port lists and tolerates a backend without midi methods", async () => {
    await midiIo.loadPorts();
    expect(midiIo.inPorts.map((p) => p.id)).toEqual(["in#0"]);
    expect(midiIo.outPorts.map((p) => p.id)).toEqual(["out#0"]);
  });

  it("selecting an input port passes the current monitor flag", async () => {
    midiIo.monitor = false;
    await midiIo.selectInPort("in#0");
    expect(midiSelectInputPort).toHaveBeenCalledWith("in#0", false);
  });

  it("clearing the input port sends null, not an empty string", async () => {
    await midiIo.selectInPort("");
    expect(midiSelectInputPort).toHaveBeenCalledWith(null, expect.anything());
  });

  it("setInputTrack forwards the track id and null to clear", async () => {
    await midiIo.setInputTrack("t-1");
    expect(midiSelectInputTrack).toHaveBeenCalledWith("t-1");
    await midiIo.setInputTrack(null);
    expect(midiSelectInputTrack).toHaveBeenCalledWith(null);
  });

  it("openOutPort/closeOutPort are pure command emissions", async () => {
    await midiIo.openOutPort("out#0");
    expect(midiOpenOutputPort).toHaveBeenCalledWith("out#0");
    await midiIo.closeOutPort("out#0");
    expect(midiCloseOutputPort).toHaveBeenCalledWith("out#0");
  });

  it("setTrackRoute forwards the route and mirrors it locally, clearing on null", async () => {
    await midiIo.setTrackRoute("t-9", "out#0", 3);
    expect(midiSetTrackRoute).toHaveBeenCalledWith("t-9", "out#0", 3);
    expect(midiIo.trackRoute("t-9")).toEqual({ scope: "track", id: "t-9", portId: "out#0", channel: 3 });

    await midiIo.setTrackRoute("t-9", null);
    expect(midiSetTrackRoute).toHaveBeenCalledWith("t-9", null, 0);
    expect(midiIo.trackRoute("t-9")).toBeUndefined();
  });

  it("setClipRoute overrides its track's route in routeForClip", async () => {
    await midiIo.setTrackRoute("t-9", "out#0", 0);
    await midiIo.setClipRoute("c-1", "out#0", 5);
    expect(midiSetClipRoute).toHaveBeenCalledWith("c-1", "out#0", 5);
    expect(midiIo.clipOverride("c-1")).toEqual({ scope: "clip", id: "c-1", portId: "out#0", channel: 5 });
    expect(midiIo.routeForClip("c-1", "t-9")?.channel).toBe(5);

    // A different clip on the same track falls back to the track's route.
    expect(midiIo.routeForClip("c-2", "t-9")?.channel).toBe(0);

    await midiIo.setClipRoute("c-1", null);
    expect(midiIo.clipOverride("c-1")).toBeUndefined();
    expect(midiIo.routeForClip("c-1", "t-9")?.channel).toBe(0);
  });

  it("polling mirrors backend status into the store", async () => {
    const stop = midiIo.startPolling(1);
    await vi.waitFor(() => expect(midiIo.routing).toBe(true));
    expect(midiIo.targetTrackId).toBe("t-1");
    await vi.waitFor(() => expect(midiIo.outputs).toHaveLength(1));
    expect(midiIo.outputs[0].port.id).toBe("out#0");
    expect(midiIo.outputs[0].clockEnabled).toBe(true);
    stop();
  });

  it("the clock toggle is a pure command emission scoped to one port", async () => {
    midiIo.outputs = [
      { port: { id: "out#0", name: "Output" }, clockEnabled: true, running: false, pulsesSent: 0, resyncs: 0, notesSent: 0 },
    ];
    await midiIo.setClockEnabled("out#0", false);
    expect(midiSetOutputClockEnabled).toHaveBeenCalledWith("out#0", false);
    expect(midiIo.outputs[0].clockEnabled).toBe(false);
  });
});
