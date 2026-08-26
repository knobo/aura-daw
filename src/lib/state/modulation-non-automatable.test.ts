/**
 * `pickTarget` refuses to MINT a lane on a plugin param the plugin itself
 * declared expensive to change / non-automatable (LV2 `pprops:expensive`,
 * `kx:NonAutomatable` — ZamVerb's "Room" carries both, because the value
 * selects the convolution impulse response and every write reloads it).
 *
 * The guard lives in the store rather than at the three surfaces that mint
 * lanes (the param panel's `A` button, a pinned chip, the automation
 * matrix), so none of them can forget it.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Binding, Curve, ModulationSnapshot, PluginParamInfo } from "../types/ipc";

const snap: ModulationSnapshot = { curves: [], bindings: [], automationClips: [] };
let nextId = 1;

function cloneSnap(): ModulationSnapshot {
  return {
    curves: snap.curves.map((c) => structuredClone(c)),
    bindings: snap.bindings.map((b) => structuredClone(b)),
    automationClips: [],
  };
}

vi.mock("../tauri", () => ({
  backend: {
    mode: "tauri",
    on: () => () => {},
    modulationGet: vi.fn(() => Promise.resolve(cloneSnap())),
    modulationSetCurve: vi.fn((curve: Curve) => {
      snap.curves.push({ ...curve, id: curve.id || `cur-${nextId++}` });
      return Promise.resolve(cloneSnap());
    }),
    modulationSetBinding: vi.fn((binding: Binding) => {
      snap.bindings.push({ ...binding, id: binding.id || `bnd-${nextId++}` });
      return Promise.resolve(cloneSnap());
    }),
    automationClipSet: vi.fn(() => Promise.resolve(cloneSnap())),
  },
}));

/** Param table the store consults through `plugins.paramInfo`. */
const params: PluginParamInfo[] = [
  // ZamVerb's real declaration: lv2:integer 0..6, expensive, NonAutomatable.
  { id: 6, name: "ZamVerb / Room", min: 0, max: 6, default: 0, value: 0, steps: 7, nonAutomatable: true },
  { id: 2, name: "ZamVerb / Dry", min: -60, max: 0, default: -6, value: -6, steps: 0 },
  // A row saved before the field existed — absent must read as automatable.
  { id: 3, name: "ZamVerb / Wet", min: -60, max: 0, default: -6, value: -6, steps: 0 },
];

vi.mock("./plugins.svelte", () => ({
  plugins: {
    paramInfo: (_instanceId: string, paramId: number) => params.find((p) => p.id === paramId),
  },
}));

const errors: { title: string; lines: string[] }[] = [];
vi.mock("./toasts.svelte", () => ({
  toasts: {
    error: (title: string, ...lines: string[]) => errors.push({ title, lines }),
    info: () => {},
    success: () => {},
  },
}));

const { modulation } = await import("./modulation.svelte");

beforeEach(() => {
  snap.curves = [];
  snap.bindings = [];
  errors.length = 0;
  nextId = 1;
  modulation.curves = [];
  modulation.bindings = [];
});

describe("pickTarget on a non-automatable plugin param", () => {
  it("mints nothing and says why", async () => {
    const out = await modulation.pickTarget("trk-1", {
      kind: "pluginParam",
      instanceId: "inst-1",
      paramId: 6,
    });
    expect(out).toBeUndefined();
    expect(snap.curves).toEqual([]);
    expect(snap.bindings).toEqual([]);
    expect(errors).toHaveLength(1);
    expect(errors[0].title).toBe("NOT AUTOMATABLE");
    expect(errors[0].lines.join(" ")).toContain("Room");
  });

  it("still mints for an ordinary param on the same plugin", async () => {
    const out = await modulation.pickTarget("trk-1", {
      kind: "pluginParam",
      instanceId: "inst-1",
      paramId: 2,
    });
    expect(out).toBeDefined();
    expect(snap.curves).toHaveLength(1);
    expect(snap.bindings).toHaveLength(1);
    expect(errors).toEqual([]);
  });

  it("mints for a param whose row predates the field (absent = automatable)", async () => {
    const out = await modulation.pickTarget("trk-1", {
      kind: "pluginParam",
      instanceId: "inst-1",
      paramId: 3,
    });
    expect(out).toBeDefined();
    expect(errors).toEqual([]);
  });

  it("still OPENS a lane that already exists on a flagged param", async () => {
    // A project saved before the backend read these properties can carry a
    // real binding on Room. Refusing to reveal it would leave the user
    // unable to see — or delete — the thing that is crackling.
    const target = { kind: "pluginParam" as const, instanceId: "inst-1", paramId: 6 };
    modulation.curves = [{ id: "cur-9", name: "Room", points: [{ tick: 0, value: 0 }] }];
    modulation.bindings = [
      { id: "bnd-9", source: { kind: "curve", curveId: "cur-9" }, target, mode: "absolute", depth: 1 },
    ];
    const out = await modulation.pickTarget("trk-1", target);
    expect(out?.id).toBe("bnd-9");
    expect(errors).toEqual([]);
  });
});
