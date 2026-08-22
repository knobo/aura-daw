import { describe, expect, it } from "vitest";
import type { Binding, PluginInstanceInfo, PluginParamInfo, TrackState } from "../types/ipc";
import { buildMatrix, matrixByParam } from "./automation-matrix";

const track = (id: string, name: string, extra: Partial<TrackState> = {}): TrackState => ({
  id,
  name,
  kind: "midi",
  gainDb: -6,
  pan: 0,
  muted: false,
  soloed: false,
  armed: false,
  automationMode: "read",
  color: "#112233",
  ...extra,
});

const inst = (id: string, uid: string, name: string): PluginInstanceInfo => ({
  id,
  uid,
  name,
  format: "clap",
  status: "active",
});

const pluginBinding = (
  id: string,
  instanceId: string,
  paramId: number,
  extra: Partial<Binding> = {},
): Binding => ({
  id,
  source: { kind: "curve", curveId: `c-${id}` },
  target: { kind: "pluginParam", instanceId, paramId },
  mode: "absolute",
  depth: 1,
  ...extra,
});

const trackParamBinding = (
  id: string,
  trackId: string,
  param: "gain" | "pan",
  extra: Partial<Binding> = {},
): Binding => ({
  id,
  source: { kind: "curve", curveId: `c-${id}` },
  target: { kind: "trackParam", trackId, param },
  mode: "absolute",
  depth: 1,
  ...extra,
});

const cutoff = (over: Partial<PluginParamInfo> = {}): PluginParamInfo => ({
  id: 12,
  name: "Filter / Cutoff",
  min: 20,
  max: 20000,
  default: 1000,
  value: 1500,
  steps: 0,
  ...over,
});

function noParamInfo(): PluginParamInfo | undefined {
  return undefined;
}

describe("buildMatrix", () => {
  it("builds a row for a plugin param on an instrument track (source: curve, target: pluginParam)", () => {
    const rows = buildMatrix({
      bindings: [pluginBinding("b1", "i1", 12)],
      instances: [inst("i1", "u-surge", "Surge XT")],
      tracks: [track("t1", "Bass", { instrumentId: "plugin:i1" })],
      visible: new Map(),
      paramInfo: (instanceId, paramId) =>
        instanceId === "i1" && paramId === 12 ? cutoff() : undefined,
    });

    expect(rows).toEqual([
      {
        bindingId: "b1",
        trackId: "t1",
        trackName: "Bass",
        instanceId: "i1",
        pluginName: "Surge XT",
        paramLabel: "Cutoff",
        valueText: "1.50khz",
        target: { kind: "pluginParam", instanceId: "i1", paramId: 12 },
        laneVisible: false,
        mode: "read",
      },
    ]);
  });

  it("resolves the lane track for a plugin param on an insert via buildRack, not by hand", () => {
    const rows = buildMatrix({
      bindings: [pluginBinding("b1", "fx1", 3)],
      instances: [inst("fx1", "u-verb", "Calf Reverb")],
      tracks: [
        track("t1", "Vox", {
          kind: "audio",
          inserts: [{ id: "s1", instanceId: "fx1", bypassed: false }],
        }),
      ],
      visible: new Map(),
      paramInfo: noParamInfo,
    });

    expect(rows).toHaveLength(1);
    expect(rows[0].trackId).toBe("t1");
    expect(rows[0].trackName).toBe("Vox");
    expect(rows[0].pluginName).toBe("Calf Reverb");
  });

  it("builds a track gain row, using formatDb", () => {
    const rows = buildMatrix({
      bindings: [trackParamBinding("b1", "t1", "gain")],
      instances: [],
      tracks: [track("t1", "Bass", { gainDb: -6 })],
      visible: new Map(),
      paramInfo: noParamInfo,
    });

    expect(rows).toEqual([
      {
        bindingId: "b1",
        trackId: "t1",
        trackName: "Bass",
        instanceId: null,
        pluginName: null,
        paramLabel: "gain",
        valueText: "-6.0",
        target: { kind: "trackParam", trackId: "t1", param: "gain" },
        laneVisible: false,
        mode: "read",
      },
    ]);
  });

  it("builds a track pan row, using the same text as TrackHeader's formatPan", () => {
    const rows = buildMatrix({
      bindings: [trackParamBinding("b1", "t1", "pan", { id: "b1" })],
      instances: [],
      tracks: [track("t1", "Bass", { pan: -0.34 })],
      visible: new Map(),
      paramInfo: noParamInfo,
    });

    expect(rows[0].paramLabel).toBe("pan");
    expect(rows[0].valueText).toBe("34L");
  });

  it("falls back to #<id> / '~' for an uncached plugin param", () => {
    const rows = buildMatrix({
      bindings: [pluginBinding("b1", "i1", 12)],
      instances: [inst("i1", "u-surge", "Surge XT")],
      tracks: [track("t1", "Bass", { instrumentId: "plugin:i1" })],
      visible: new Map(),
      paramInfo: noParamInfo,
    });

    expect(rows[0].paramLabel).toBe("#12");
    expect(rows[0].valueText).toBe("~");
  });

  it("drops a clipEnvelope binding — not a lane, nothing to reveal", () => {
    const rows = buildMatrix({
      bindings: [
        {
          id: "b1",
          source: { kind: "clipEnvelope", contentId: "clip-1", curveId: "c1" },
          target: { kind: "pluginParam", instanceId: "i1", paramId: 12 },
          mode: "absolute",
          depth: 1,
        },
      ],
      instances: [inst("i1", "u-surge", "Surge XT")],
      tracks: [track("t1", "Bass", { instrumentId: "plugin:i1" })],
      visible: new Map(),
      paramInfo: noParamInfo,
    });

    expect(rows).toEqual([]);
  });

  it("drops an out-of-scope target kind (macro) — arrives with modulation §8", () => {
    const rows = buildMatrix({
      bindings: [
        {
          id: "b1",
          source: { kind: "automationTrack", trackId: "t1" },
          target: { kind: "macro", macroId: "m1" },
          mode: "absolute",
          depth: 1,
        },
      ],
      instances: [],
      tracks: [track("t1", "Bass")],
      visible: new Map(),
      paramInfo: noParamInfo,
    });

    expect(rows).toEqual([]);
  });

  it("drops a binding whose track cannot be resolved instead of inventing a placeholder", () => {
    const rows = buildMatrix({
      bindings: [pluginBinding("b1", "orphan-1", 1)],
      instances: [inst("orphan-1", "u-x", "Orphan FX")],
      tracks: [],
      visible: new Map(),
      paramInfo: noParamInfo,
    });

    expect(rows).toEqual([]);
  });

  it("resolves an automationTrack-sourced binding to source.trackId, not the target's", () => {
    const rows = buildMatrix({
      bindings: [
        {
          id: "b1",
          source: { kind: "automationTrack", trackId: "auto-1" },
          target: { kind: "pluginParam", instanceId: "i1", paramId: 12 },
          mode: "absolute",
          depth: 1,
        },
      ],
      instances: [inst("i1", "u-surge", "Surge XT")],
      tracks: [
        track("auto-1", "Auto Lane", { kind: "automation" }),
        track("t1", "Bass", { instrumentId: "plugin:i1" }),
      ],
      visible: new Map(),
      paramInfo: () => cutoff(),
    });

    expect(rows).toHaveLength(1);
    expect(rows[0].trackId).toBe("auto-1");
    expect(rows[0].trackName).toBe("Auto Lane");
  });

  it("laneVisible reflects the visible map, true and false", () => {
    const rows = buildMatrix({
      bindings: [
        pluginBinding("b1", "i1", 12),
        pluginBinding("b2", "i1", 13),
      ],
      instances: [inst("i1", "u-surge", "Surge XT")],
      tracks: [track("t1", "Bass", { instrumentId: "plugin:i1" })],
      visible: new Map([["t1", new Set(["b1"])]]),
      paramInfo: () => cutoff(),
    });

    const b1 = rows.find((r) => r.bindingId === "b1");
    const b2 = rows.find((r) => r.bindingId === "b2");
    expect(b1?.laneVisible).toBe(true);
    expect(b2?.laneVisible).toBe(false);
  });

  it("sorts by paramLabel, then trackName, then pluginName", () => {
    const rows = buildMatrix({
      bindings: [
        pluginBinding("b-zeta-cutoff", "i-zeta", 12),
        pluginBinding("b-alpha-cutoff", "i-alpha", 12),
        trackParamBinding("b-gain", "t1", "gain"),
      ],
      instances: [inst("i-zeta", "u-z", "Zeta"), inst("i-alpha", "u-a", "Alpha")],
      tracks: [
        track("t1", "Bass", {
          instrumentId: "plugin:i-alpha",
        }),
        track("t2", "Lead", { instrumentId: "plugin:i-zeta" }),
      ],
      visible: new Map(),
      paramInfo: () => cutoff(),
    });

    expect(rows.map((r) => r.bindingId)).toEqual(["b-alpha-cutoff", "b-zeta-cutoff", "b-gain"]);
  });
});

describe("matrixByParam", () => {
  it("groups two different plugins' 'Cutoff' params into one bucket, in first-appearance order", () => {
    const rows = buildMatrix({
      bindings: [
        trackParamBinding("b-gain", "t1", "gain"),
        pluginBinding("b-alpha-cutoff", "i-alpha", 12),
        pluginBinding("b-zeta-cutoff", "i-zeta", 12),
      ],
      instances: [inst("i-alpha", "u-a", "Alpha"), inst("i-zeta", "u-z", "Zeta")],
      tracks: [
        track("t1", "Bass", { instrumentId: "plugin:i-alpha" }),
        track("t2", "Lead", { instrumentId: "plugin:i-zeta" }),
      ],
      visible: new Map(),
      paramInfo: () => cutoff(),
    });

    const groups = matrixByParam(rows);
    expect(groups.map((g) => g.label)).toEqual(["Cutoff", "gain"]);
    expect(groups[0].rows.map((r) => r.bindingId)).toEqual([
      "b-alpha-cutoff",
      "b-zeta-cutoff",
    ]);
    expect(groups[1].rows.map((r) => r.bindingId)).toEqual(["b-gain"]);
  });
});
