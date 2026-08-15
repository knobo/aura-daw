/**
 * Modulation store: a thin mirror of the backend's `modulation{}`
 * (ADR 0006 — no authoritative state here). `preview` is the drag-time
 * local patch; `commit` is the one invoke per gesture.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { AutomationClip, Binding, Curve, ModulationSnapshot } from "../types/ipc";

const snap: ModulationSnapshot = { curves: [], bindings: [], automationClips: [] };
const setCurvePayloads: Curve[] = [];
let nextId = 1;

function cloneSnap(): ModulationSnapshot {
  return {
    curves: snap.curves.map((c) => structuredClone(c)),
    bindings: snap.bindings.map((b) => structuredClone(b)),
    automationClips: snap.automationClips.map((a) => structuredClone(a)),
  };
}

const modulationGet = vi.fn(() => Promise.resolve(cloneSnap()));
const modulationSetCurve = vi.fn((curve: Curve) => {
  setCurvePayloads.push(curve);
  const minted = { ...curve, id: curve.id || `cur-${nextId++}` };
  const i = snap.curves.findIndex((c) => c.id === minted.id);
  if (i >= 0) snap.curves[i] = minted;
  else snap.curves.push(minted);
  return Promise.resolve(cloneSnap());
});
const modulationSetBinding = vi.fn((binding: Binding, del?: boolean) => {
  if (del) {
    snap.bindings = snap.bindings.filter((b) => b.id !== binding.id);
    return Promise.resolve(cloneSnap());
  }
  const minted = { ...binding, id: binding.id || `bnd-${nextId++}` };
  const i = snap.bindings.findIndex((b) => b.id === minted.id);
  if (i >= 0) snap.bindings[i] = minted;
  else snap.bindings.push(minted);
  return Promise.resolve(cloneSnap());
});
const automationClipSet = vi.fn((clip: AutomationClip, del?: boolean) => {
  if (del) {
    snap.automationClips = snap.automationClips.filter((c) => c.id !== clip.id);
    return Promise.resolve(cloneSnap());
  }
  const minted = { ...clip, id: clip.id || `acl-${nextId++}` };
  const i = snap.automationClips.findIndex((c) => c.id === minted.id);
  if (i >= 0) snap.automationClips[i] = minted;
  else snap.automationClips.push(minted);
  return Promise.resolve(cloneSnap());
});

vi.mock("../tauri", () => ({
  backend: {
    mode: "tauri",
    on: () => () => {},
    modulationGet,
    modulationSetCurve,
    modulationSetBinding,
    automationClipSet,
  },
}));

const { modulation } = await import("./modulation.svelte");

const curve = (id: string): Curve => ({
  id,
  name: "gain",
  points: [
    { tick: 0, value: 1 },
    { tick: 3840, value: 0 },
  ],
});

const binding = (id: string, curveId: string, trackId: string): Binding => ({
  id,
  source: { kind: "curve", curveId },
  target: { kind: "trackParam", trackId, param: "gain" },
  mode: "multiply",
  depth: 1,
});

beforeEach(() => {
  vi.clearAllMocks();
  snap.curves.length = 0;
  snap.bindings.length = 0;
  snap.automationClips.length = 0;
  setCurvePayloads.length = 0;
  nextId = 1;
  modulation.curves = [];
  modulation.bindings = [];
  modulation.automationClips = [];
  modulation.visible = new Map();
});

describe("modulation store", () => {
  it("mirrors curves and bindings from the backend", async () => {
    snap.curves.push(curve("cur-1"));
    snap.bindings.push(binding("bnd-1", "cur-1", "t-1"));
    await modulation.reload();
    expect(modulation.curves).toHaveLength(1);
    expect(modulation.bindings).toHaveLength(1);
    expect(modulation.curves[0].id).toBe("cur-1");
    expect(modulation.bindingsFor({ kind: "trackParam", trackId: "t-1", param: "gain" })).toHaveLength(1);
    expect(modulation.bindingsFor({ kind: "trackParam", trackId: "t-2", param: "gain" })).toHaveLength(0);
  });

  it("preview patches points locally without invoking", async () => {
    modulation.curves = [curve("cur-1")];
    modulation.preview("cur-1", [{ tick: 0, value: 0.5 }]);
    expect(modulation.curves[0].points).toEqual([{ tick: 0, value: 0.5 }]);
    expect(modulationSetCurve).not.toHaveBeenCalled();
  });

  it("commit sends the whole curve once", async () => {
    const c = curve("cur-1");
    await modulation.commit(c);
    expect(modulationSetCurve).toHaveBeenCalledTimes(1);
    expect(modulationSetCurve).toHaveBeenCalledWith(c);
    expect(setCurvePayloads).toHaveLength(1);
    expect(setCurvePayloads[0]).toEqual(c);
    expect(modulation.curves).toHaveLength(1);
    expect(modulation.curves[0].id).toBe("cur-1");
  });
});

describe("automation track routing", () => {
  it("adding a target mints a binding sourced from the automation track", async () => {
    const b = await modulation.addTarget("auto", {
      kind: "trackParam",
      trackId: "t-1",
      param: "gain",
    });
    expect(b).toBeDefined();
    expect(modulationSetBinding).toHaveBeenCalledTimes(1);
    expect(modulation.bindings).toHaveLength(1);
    expect(modulation.bindings[0].source).toEqual({
      kind: "automationTrack",
      trackId: "auto",
    });
    expect(modulation.bindings[0].target).toEqual({
      kind: "trackParam",
      trackId: "t-1",
      param: "gain",
    });
    expect(modulation.bindingsFrom("auto")).toHaveLength(1);
    expect(modulation.bindingsFrom("other")).toHaveLength(0);
  });

  it("one automation track bound to two targets keeps independent depths", async () => {
    await modulation.addTarget("auto", { kind: "trackParam", trackId: "t-1", param: "gain" });
    await modulation.addTarget("auto", { kind: "trackParam", trackId: "t-2", param: "gain" });
    const both = modulation.bindingsFrom("auto");
    expect(both).toHaveLength(2);
    expect(both.map((b) => (b.target.kind === "trackParam" ? b.target.trackId : ""))).toEqual([
      "t-1",
      "t-2",
    ]);
    await modulation.setDepth(both[0], 0.25);
    expect(modulation.bindingsFrom("auto")[0].depth).toBe(0.25);
    expect(modulation.bindingsFrom("auto")[1].depth).toBe(1);
  });

  it("removing a target deletes the binding", async () => {
    await modulation.addTarget("auto", { kind: "trackParam", trackId: "t-1", param: "gain" });
    const id = modulation.bindingsFrom("auto")[0].id;
    await modulation.removeBinding(id);
    expect(modulation.bindingsFrom("auto")).toHaveLength(0);
    expect(modulation.bindings).toHaveLength(0);
  });

  it("dropping a track locally removes its clips and sourced bindings", async () => {
    await modulation.addTarget("auto", { kind: "trackParam", trackId: "t-1", param: "gain" });
    await modulation.addClip("auto", 0, 3840);
    await modulation.addTarget("other", { kind: "trackParam", trackId: "t-2", param: "gain" });
    modulation.dropTrack("auto");
    expect(modulation.clipsOn("auto")).toHaveLength(0);
    expect(modulation.bindingsFrom("auto")).toHaveLength(0);
    expect(modulation.bindingsFrom("other")).toHaveLength(1);
  });

  it("adding a clip persists it on the automation track", async () => {
    const clip = await modulation.addClip("auto", 0, 3840);
    expect(clip).toBeDefined();
    expect(modulation.clipsOn("auto")).toHaveLength(1);
    expect(modulation.clipsOn("auto")[0].trackId).toBe("auto");
    expect(modulation.clipsOn("auto")[0].lengthTicks).toBe(3840);
    expect(modulation.clipsOn("auto")[0].curveId).toBeTruthy();
    expect(modulation.curves.some((c) => c.id === modulation.clipsOn("auto")[0].curveId)).toBe(
      true,
    );
  });
});
