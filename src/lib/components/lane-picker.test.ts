/**
 * Lane picker + overlay contract (Task 8). The repo has no DOM test
 * environment, so these exercise the store methods the header menu and
 * Timeline overlay loop delegate to.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Binding, Curve, ModulationSnapshot, TargetRef } from "../types/ipc";

const snap: ModulationSnapshot = { curves: [], bindings: [], automationClips: [] };
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
  const minted = { ...curve, id: curve.id || `cur-${nextId++}` };
  const i = snap.curves.findIndex((c) => c.id === minted.id);
  if (i >= 0) snap.curves[i] = minted;
  else snap.curves.push(minted);
  return Promise.resolve(cloneSnap());
});
const modulationSetBinding = vi.fn((binding: Binding) => {
  const minted = { ...binding, id: binding.id || `bnd-${nextId++}` };
  const i = snap.bindings.findIndex((b) => b.id === minted.id);
  if (i >= 0) snap.bindings[i] = minted;
  else snap.bindings.push(minted);
  return Promise.resolve(cloneSnap());
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

const { modulation, panNativeOf } = await import("../state/modulation.svelte");

beforeEach(() => {
  vi.clearAllMocks();
  snap.curves.length = 0;
  snap.bindings.length = 0;
  snap.automationClips.length = 0;
  nextId = 1;
  modulation.curves = [];
  modulation.bindings = [];
  modulation.automationClips = [];
  modulation.visible = new Map();
});

function gainTarget(trackId: string): TargetRef {
  return { kind: "trackParam", trackId, param: "gain" };
}
function panTarget(trackId: string): TargetRef {
  return { kind: "trackParam", trackId, param: "pan" };
}

describe("lane picker", () => {
  it("picking a target that has no curve mints a curve and a binding", async () => {
    expect(modulation.curves).toHaveLength(0);
    expect(modulation.bindings).toHaveLength(0);

    const binding = await modulation.pickTarget("t-1", gainTarget("t-1"));
    expect(binding).toBeDefined();
    if (!binding) throw new Error("expected a minted binding");

    expect(modulationSetCurve).toHaveBeenCalledTimes(1);
    expect(modulationSetBinding).toHaveBeenCalledTimes(1);
    expect(modulation.curves).toHaveLength(1);
    expect(modulation.bindings).toHaveLength(1);
    expect(modulation.curves[0].id).not.toBe("");
    expect(modulation.bindings[0].id).not.toBe("");
    expect(binding.id).toBe(modulation.bindings[0].id);
    expect(modulation.bindings[0].source).toEqual({
      kind: "curve",
      curveId: modulation.curves[0].id,
    });
    expect(modulation.bindings[0].target).toEqual(gainTarget("t-1"));
  });

  it("a track shows one overlay per visible binding, each editing its own curve", async () => {
    await modulation.pickTarget("t-1", gainTarget("t-1"));
    await modulation.pickTarget("t-1", panTarget("t-1"));

    const overlays = modulation.visibleBindingsFor("t-1");
    expect(overlays).toHaveLength(2);
    const curves = overlays.map((b) => modulation.curveOf(b)!);
    expect(curves[0].id).not.toBe(curves[1].id);
    expect(curves.every((c) => c != null)).toBe(true);

    const before = curves[1].points.slice();
    modulation.preview(curves[0].id, [{ tick: 0, value: 0.2 }]);
    expect(modulation.curveOf(overlays[0])!.points).toEqual([{ tick: 0, value: 0.2 }]);
    expect(modulation.curveOf(overlays[1])!.points).toEqual(before);
  });

  it("pan lane values map 0..1 to hard left..hard right", () => {
    expect(panNativeOf(0)).toBeCloseTo(-1);
    expect(panNativeOf(0.5)).toBeCloseTo(0);
    expect(panNativeOf(1)).toBeCloseTo(1);
  });
});
