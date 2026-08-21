import { describe, expect, it } from "vitest";
import type { Binding, PluginInstanceInfo, PluginParamInfo, TrackState } from "../types/ipc";
import { buildLaneStrip, fitLaneStrip, type StripDevice } from "./lane-strip";

const inst = (
  id: string,
  uid: string,
  name: string,
  status: PluginInstanceInfo["status"] = "active",
): PluginInstanceInfo => ({ id, uid, name, format: "clap", status });

const track = (extra: Partial<TrackState> = {}): TrackState => ({
  id: "t1",
  name: "Bass",
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

const param = (id: number, name: string, value: number): PluginParamInfo => ({
  id,
  name,
  min: 0,
  max: 1,
  default: 0,
  value,
});

const noPinned = () => [] as number[];
const noParamInfo = () => undefined;

describe("buildLaneStrip", () => {
  it("puts the plugin instrument first, then inserts in slot order", () => {
    const devices = buildLaneStrip({
      track: track({
        instrumentId: "plugin:synth-1",
        inserts: [
          { id: "s1", instanceId: "fx-1", bypassed: false },
          { id: "s2", instanceId: "fx-2", bypassed: false },
        ],
      }),
      instances: [
        inst("fx-2", "u-b", "Insert B"),
        inst("synth-1", "u-a", "Synth"),
        inst("fx-1", "u-c", "Insert A"),
      ],
      bindings: [],
      pinnedFor: noPinned,
      paramInfo: noParamInfo,
    });

    expect(devices.map((d) => [d.kind, d.name, d.slotIndex])).toEqual([
      ["instrument", "Synth", undefined],
      ["insert", "Insert A", 0],
      ["insert", "Insert B", 1],
    ]);
  });

  it("a sampler instrument (non-plugin) contributes no device", () => {
    const devices = buildLaneStrip({
      track: track({ instrumentId: "sampler:kit-1" }),
      instances: [inst("synth-1", "u-a", "Synth")],
      bindings: [],
      pinnedFor: noPinned,
      paramInfo: noParamInfo,
    });

    expect(devices).toEqual([]);
  });

  it("skips an insert slot whose instance is not live", () => {
    const devices = buildLaneStrip({
      track: track({
        inserts: [
          { id: "s1", instanceId: "dead-1", bypassed: false },
          { id: "s2", instanceId: "fx-1", bypassed: false },
        ],
      }),
      instances: [inst("fx-1", "u-c", "Insert A")],
      bindings: [],
      pinnedFor: noPinned,
      paramInfo: noParamInfo,
    });

    expect(devices.map((d) => d.name)).toEqual(["Insert A"]);
  });

  it("orders chips pinned-first then automated, de-duplicated", () => {
    const devices = buildLaneStrip({
      track: track({ instrumentId: "plugin:synth-1" }),
      instances: [inst("synth-1", "u-a", "Synth")],
      bindings: [curveBinding("b1", "synth-1", 2), curveBinding("b2", "synth-1", 5)],
      pinnedFor: (uid) => (uid === "u-a" ? [5, 1] : []),
      paramInfo: (instanceId, paramId) =>
        instanceId === "synth-1"
          ? param(paramId, `Param ${paramId}`, 0.5)
          : undefined,
    });

    expect(devices[0].chips.map((c) => c.paramId)).toEqual([5, 1, 2]);
    // param 5 is both pinned AND automated -> automated state, not duplicated.
    expect(devices[0].chips[0].state).toBe("automated");
    expect(devices[0].chips[1].state).toBe("plain");
    expect(devices[0].chips[2].state).toBe("automated");
  });

  it("falls back to #id / '~' for an uncached param", () => {
    const devices = buildLaneStrip({
      track: track({ instrumentId: "plugin:synth-1" }),
      instances: [inst("synth-1", "u-a", "Synth")],
      bindings: [curveBinding("b1", "synth-1", 12)],
      pinnedFor: noPinned,
      paramInfo: noParamInfo,
    });

    expect(devices[0].chips).toEqual([
      { paramId: 12, label: "#12", valueText: "~", state: "automated" },
    ]);
  });

  it("does not mutate its inputs", () => {
    const bindings = [curveBinding("b1", "synth-1", 1)];
    const frozenBindings = JSON.parse(JSON.stringify(bindings));
    buildLaneStrip({
      track: track({ instrumentId: "plugin:synth-1" }),
      instances: [inst("synth-1", "u-a", "Synth")],
      bindings,
      pinnedFor: noPinned,
      paramInfo: noParamInfo,
    });
    expect(bindings).toEqual(frozenBindings);
  });
});

describe("fitLaneStrip", () => {
  const fourDevices: StripDevice[] = [
    { instanceId: "i1", name: "A", status: "active", kind: "instrument", bypassed: false, chips: [
      { paramId: 1, label: "P1", valueText: "1", state: "plain" },
      { paramId: 2, label: "P2", valueText: "2", state: "plain" },
      { paramId: 3, label: "P3", valueText: "3", state: "plain" },
    ] },
    { instanceId: "i2", name: "B", status: "active", kind: "insert", slotIndex: 0, bypassed: false, chips: [] },
    { instanceId: "i3", name: "C", status: "active", kind: "insert", slotIndex: 1, bypassed: false, chips: [] },
    { instanceId: "i4", name: "D", status: "active", kind: "insert", slotIndex: 2, bypassed: false, chips: [] },
  ];

  it("caps at maxEntries and reports overflow", () => {
    const fit = fitLaneStrip(fourDevices, { maxEntries: 2, chipsPerEntry: 2 });
    expect(fit.shown.map((d) => d.name)).toEqual(["A", "B"]);
    expect(fit.overflow).toBe(2);
  });

  it("trims chips per entry without dropping the chip count budget", () => {
    const fit = fitLaneStrip(fourDevices, { maxEntries: 2, chipsPerEntry: 2 });
    expect(fit.shown[0].chips.map((c) => c.paramId)).toEqual([1, 2]);
  });

  it("chipsPerEntry: 0 strips every chip but keeps all devices under maxEntries: 4", () => {
    const fit = fitLaneStrip(fourDevices, { maxEntries: 4, chipsPerEntry: 0 });
    expect(fit.shown.map((d) => d.name)).toEqual(["A", "B", "C", "D"]);
    expect(fit.shown.every((d) => d.chips.length === 0)).toBe(true);
    expect(fit.overflow).toBe(0);
  });

  it("does not mutate the input array or its devices", () => {
    const before = JSON.parse(JSON.stringify(fourDevices));
    fitLaneStrip(fourDevices, { maxEntries: 2, chipsPerEntry: 1 });
    expect(fourDevices).toEqual(before);
  });
});
