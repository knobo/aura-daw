/**
 * Modulation store: a thin mirror of the backend's `modulation{}`
 * (ADR 0006 — no authoritative state here). `preview` is the drag-time
 * local patch; `commit` is the one invoke per gesture.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Binding, Curve, ModulationSnapshot } from "../types/ipc";

const snap: ModulationSnapshot = { curves: [], bindings: [], automationClips: [] };
const setCurvePayloads: Curve[] = [];

const modulationGet = vi.fn(() => Promise.resolve({ ...snap, curves: [...snap.curves], bindings: [...snap.bindings], automationClips: [...snap.automationClips] }));
const modulationSetCurve = vi.fn((curve: Curve) => {
  setCurvePayloads.push(curve);
  const i = snap.curves.findIndex((c) => c.id === curve.id);
  if (i >= 0) snap.curves[i] = curve;
  else snap.curves.push(curve);
  return Promise.resolve({
    curves: [...snap.curves],
    bindings: [...snap.bindings],
    automationClips: [...snap.automationClips],
  });
});
const modulationSetBinding = vi.fn((binding: Binding) => {
  const i = snap.bindings.findIndex((b) => b.id === binding.id);
  if (i >= 0) snap.bindings[i] = binding;
  else snap.bindings.push(binding);
  return Promise.resolve({
    curves: [...snap.curves],
    bindings: [...snap.bindings],
    automationClips: [...snap.automationClips],
  });
});

vi.mock("../tauri", () => ({
  backend: {
    mode: "tauri",
    on: () => () => {},
    modulationGet,
    modulationSetCurve,
    modulationSetBinding,
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
  modulation.curves = [];
  modulation.bindings = [];
  modulation.automationClips = [];
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
