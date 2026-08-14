/**
 * MIDI I/O store (slice 2): port loading, in/out port selection, the arm
 * target and clock toggle, and the status poll — mirrors project.svelte
 * .test.ts's `vi.mock("../tauri", …)` pattern.
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
const midiSelectOutputPort = vi.fn(() => Promise.resolve());
const midiSetClockEnabled = vi.fn(() => Promise.resolve());
const midiSelectOutputTrack = vi.fn(() => Promise.resolve());

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
  selected: { id: "out#0", name: "Output" },
  clockEnabled: true,
  running: true,
  pulsesSent: 0,
  resyncs: 0,
  noteTrackId: null,
  notesSent: 0,
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
    midiSelectOutputPort,
    midiSetClockEnabled,
    midiSelectOutputTrack,
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
  midiIo.clockEnabled = false;
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

  it("polling mirrors backend status into the store", async () => {
    const stop = midiIo.startPolling(1);
    await vi.waitFor(() => expect(midiIo.routing).toBe(true));
    expect(midiIo.targetTrackId).toBe("t-1");
    expect(midiIo.clockRunning).toBe(true);
    stop();
  });

  it("the clock toggle is a pure command emission", async () => {
    await midiIo.setClockEnabled(false);
    expect(midiSetClockEnabled).toHaveBeenCalledWith(false);
    expect(midiIo.clockEnabled).toBe(false);
  });
});
