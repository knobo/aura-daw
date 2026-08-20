import { describe, expect, it } from "vitest";
import type { Binding, PluginDescriptor, PluginInstanceInfo, TrackState } from "../types/ipc";
import { buildRack, rackByTrack, rackCounts, type RackEntry } from "./plugin-rack";

const desc = (uid: string, name: string, extra: Partial<PluginDescriptor> = {}): PluginDescriptor => ({
  uid,
  format: "clap",
  name,
  isInstrument: true,
  audioInputs: 0,
  audioOutputs: 2,
  hasNoteInput: true,
  ...extra,
});

const inst = (
  id: string,
  uid: string,
  name: string,
  status: PluginInstanceInfo["status"] = "active",
): PluginInstanceInfo => ({ id, uid, name, format: "clap", status });

const track = (id: string, name: string, extra: Partial<TrackState> = {}): TrackState => ({
  id,
  name,
  kind: "midi",
  gainDb: 0,
  pan: 0,
  muted: false,
  soloed: false,
  armed: false,
  automationMode: "read",
  color: "#112233",
  ...extra,
});

const curveBinding = (id: string, instanceId: string, paramId: number): Binding => ({
  id,
  source: { kind: "curve", curveId: `c-${id}` },
  target: { kind: "pluginParam", instanceId, paramId },
  mode: "absolute",
  depth: 1,
});

const entryFor = (rack: RackEntry[], instanceId: string): RackEntry => {
  const found = rack.find((e) => e.instance.id === instanceId);
  if (!found) throw new Error(`no rack entry for ${instanceId}`);
  return found;
};

describe("buildRack", () => {
  it("gives an instrument-bound instance one instrument placement", () => {
    const rack = buildRack({
      instances: [inst("i1", "clap:surge", "Surge XT")],
      descriptors: [desc("clap:surge", "Surge XT")],
      tracks: [track("t1", "Bass", { instrumentId: "plugin:i1" })],
      bindings: [],
    });

    expect(entryFor(rack, "i1").placements).toEqual([
      { kind: "instrument", trackId: "t1", trackName: "Bass" },
    ]);
  });

  it("gives an inserted instance an insert placement carrying its slot index and bypass", () => {
    const rack = buildRack({
      instances: [inst("i2", "clap:verb", "Calf Reverb")],
      descriptors: [desc("clap:verb", "Calf Reverb", { isInstrument: false })],
      tracks: [
        track("t1", "Vox", {
          kind: "audio",
          inserts: [
            { id: "s0", instanceId: "other", bypassed: false },
            { id: "s1", instanceId: "i2", bypassed: true },
          ],
        }),
      ],
      bindings: [],
    });

    expect(entryFor(rack, "i2").placements).toEqual([
      { kind: "insert", trackId: "t1", trackName: "Vox", slotIndex: 1, bypassed: true },
    ]);
  });

  it("reports every placement when one instance sits on more than one track", () => {
    const rack = buildRack({
      instances: [inst("i1", "clap:verb", "Calf Reverb")],
      descriptors: [],
      tracks: [
        track("t1", "Drums", { kind: "audio", inserts: [{ id: "s0", instanceId: "i1", bypassed: false }] }),
        track("t2", "Vox", { kind: "audio", inserts: [{ id: "s0", instanceId: "i1", bypassed: false }] }),
      ],
      bindings: [],
    });

    expect(entryFor(rack, "i1").placements.map((p) => p.trackName)).toEqual(["Drums", "Vox"]);
  });

  it("leaves an instance nothing points at with no placements — an orphan", () => {
    const rack = buildRack({
      instances: [inst("i9", "clap:surge", "Surge XT")],
      descriptors: [],
      tracks: [track("t1", "Bass", { instrumentId: "plugin:someone-else" })],
      bindings: [],
    });

    expect(entryFor(rack, "i9").placements).toEqual([]);
  });

  it("attaches the descriptor for a scanned uid and leaves it undefined for an unscanned one", () => {
    const rack = buildRack({
      instances: [inst("i1", "clap:surge", "Surge XT"), inst("i2", "clap:ghost", "Ghost")],
      descriptors: [desc("clap:surge", "Surge XT")],
      tracks: [],
      bindings: [],
    });

    expect(entryFor(rack, "i1").descriptor?.uid).toBe("clap:surge");
    expect(entryFor(rack, "i2").descriptor).toBeUndefined();
  });

  it("lists the params a curve binding automates, in param-id order", () => {
    const rack = buildRack({
      instances: [inst("i1", "clap:surge", "Surge XT")],
      descriptors: [],
      tracks: [],
      bindings: [curveBinding("b2", "i1", 7), curveBinding("b1", "i1", 3)],
    });

    expect(entryFor(rack, "i1").automated).toEqual([
      { instanceId: "i1", paramId: 3 },
      { instanceId: "i1", paramId: 7 },
    ]);
  });

  it("counts a param automated once even when two bindings target it", () => {
    const rack = buildRack({
      instances: [inst("i1", "clap:surge", "Surge XT")],
      descriptors: [],
      tracks: [],
      bindings: [curveBinding("b1", "i1", 3), curveBinding("b2", "i1", 3)],
    });

    expect(entryFor(rack, "i1").automated).toEqual([{ instanceId: "i1", paramId: 3 }]);
  });

  it("ignores bindings aimed at a different instance or at a track param", () => {
    const rack = buildRack({
      instances: [inst("i1", "clap:surge", "Surge XT")],
      descriptors: [],
      tracks: [],
      bindings: [
        curveBinding("b1", "other", 3),
        {
          id: "b2",
          source: { kind: "curve", curveId: "c" },
          target: { kind: "trackParam", trackId: "t1", param: "gain" },
          mode: "absolute",
          depth: 1,
        },
      ],
    });

    expect(entryFor(rack, "i1").automated).toEqual([]);
  });

  it("leaves mapped empty — nothing binds a MIDI controller yet", () => {
    const rack = buildRack({
      instances: [inst("i1", "clap:surge", "Surge XT")],
      descriptors: [],
      tracks: [],
      bindings: [curveBinding("b1", "i1", 3)],
    });

    expect(entryFor(rack, "i1").mapped).toEqual([]);
  });

  it("orders entries by track order, and puts orphans last", () => {
    const rack = buildRack({
      instances: [
        inst("orphan", "clap:ghost", "Ghost"),
        inst("i2", "clap:verb", "Reverb"),
        inst("i1", "clap:surge", "Surge XT"),
      ],
      descriptors: [],
      tracks: [
        track("t1", "Bass", { instrumentId: "plugin:i1" }),
        track("t2", "Vox", { kind: "audio", inserts: [{ id: "s0", instanceId: "i2", bypassed: false }] }),
      ],
      bindings: [],
    });

    expect(rack.map((e) => e.instance.id)).toEqual(["i1", "i2", "orphan"]);
  });

  it("orders an instrument before the inserts of the same track", () => {
    const rack = buildRack({
      instances: [inst("fx", "clap:verb", "Reverb"), inst("synth", "clap:surge", "Surge XT")],
      descriptors: [],
      tracks: [
        track("t1", "Bass", {
          instrumentId: "plugin:synth",
          inserts: [{ id: "s0", instanceId: "fx", bypassed: false }],
        }),
      ],
      bindings: [],
    });

    expect(rack.map((e) => e.instance.id)).toEqual(["synth", "fx"]);
  });
});

describe("rackCounts", () => {
  it("counts stubs, crashed instances and orphans over the whole rack", () => {
    const rack = buildRack({
      instances: [
        inst("i1", "u", "A", "stub"),
        inst("i2", "u", "B", "crashed"),
        inst("i3", "u", "C", "active"),
        inst("i4", "u", "D", "stub"),
      ],
      descriptors: [],
      tracks: [track("t1", "Bass", { instrumentId: "plugin:i3" })],
      bindings: [],
    });

    expect(rackCounts(rack)).toEqual({ total: 4, stub: 2, crashed: 1, orphans: 3, automated: 0 });
  });

  it("counts every automated parameter across instances, not the instances themselves", () => {
    const rack = buildRack({
      instances: [inst("i1", "u", "A"), inst("i2", "u", "B")],
      descriptors: [],
      tracks: [],
      bindings: [curveBinding("b1", "i1", 1), curveBinding("b2", "i1", 2), curveBinding("b3", "i2", 1)],
    });

    expect(rackCounts(rack).automated).toBe(3);
  });
});

describe("rackByTrack", () => {
  it("groups entries under the track they sit on, in track order", () => {
    const rack = buildRack({
      instances: [inst("i1", "u", "Surge"), inst("i2", "u", "Reverb")],
      descriptors: [],
      tracks: [
        track("t1", "Bass", { instrumentId: "plugin:i1" }),
        track("t2", "Vox", { kind: "audio", inserts: [{ id: "s0", instanceId: "i2", bypassed: false }] }),
      ],
      bindings: [],
    });

    expect(rackByTrack(rack).map((g) => [g.trackId, g.entries.map((e) => e.instance.id)])).toEqual([
      ["t1", ["i1"]],
      ["t2", ["i2"]],
    ]);
  });

  it("repeats an instance under every track it sits on", () => {
    const rack = buildRack({
      instances: [inst("i1", "u", "Reverb")],
      descriptors: [],
      tracks: [
        track("t1", "Drums", { kind: "audio", inserts: [{ id: "s0", instanceId: "i1", bypassed: false }] }),
        track("t2", "Vox", { kind: "audio", inserts: [{ id: "s0", instanceId: "i1", bypassed: false }] }),
      ],
      bindings: [],
    });

    expect(rackByTrack(rack).map((g) => g.trackId)).toEqual(["t1", "t2"]);
  });

  it("lists an instance once per track even when it holds two slots there", () => {
    const rack = buildRack({
      instances: [inst("i1", "u", "Reverb")],
      descriptors: [],
      tracks: [
        track("t1", "Vox", {
          kind: "audio",
          inserts: [
            { id: "s0", instanceId: "i1", bypassed: false },
            { id: "s1", instanceId: "i1", bypassed: false },
          ],
        }),
      ],
      bindings: [],
    });

    expect(rackByTrack(rack)[0].entries).toHaveLength(1);
  });

  it("collects orphans in a trailing group with no track id", () => {
    const rack = buildRack({
      instances: [inst("i1", "u", "Surge"), inst("orphan", "u", "Ghost")],
      descriptors: [],
      tracks: [track("t1", "Bass", { instrumentId: "plugin:i1" })],
      bindings: [],
    });

    const groups = rackByTrack(rack);
    expect(groups.at(-1)).toMatchObject({
      trackId: null,
      trackName: "Not on any track",
    });
    expect(groups.at(-1)?.entries.map((e) => e.instance.id)).toEqual(["orphan"]);
  });

  it("omits the orphan group entirely when every instance is placed", () => {
    const rack = buildRack({
      instances: [inst("i1", "u", "Surge")],
      descriptors: [],
      tracks: [track("t1", "Bass", { instrumentId: "plugin:i1" })],
      bindings: [],
    });

    expect(rackByTrack(rack).every((g) => g.trackId !== null)).toBe(true);
  });
});
