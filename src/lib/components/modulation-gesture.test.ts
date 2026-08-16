/**
 * Modulation lane overlay gesture sequencing — port of
 * `src/lib/state/automation-gesture.test.ts`.
 *
 * `ModulationLaneView`'s pointerdown inserts a point and commits WITHOUT
 * awaiting; its pointerup then re-reads the store and commits that. The
 * store is only written when `modulation_set_curve` RESOLVES, so a
 * pointerup landing inside the insert's round-trip window reads the
 * PRE-insert curve and commits it back over the fresh point — last arrival
 * wins and the point vanishes. Both commits fold into one gesture, so no
 * undo entry reveals the loss.
 *
 * This exercises the two store methods the handlers delegate to
 * (`commitInGesture` = pointerdown, `commitLatest` = pointerup), because
 * the repo has no DOM test environment to mount the component in.
 *
 * Barriers are keyed by bindingId (not trackId): two lanes on one track
 * make the old store-wide / track-keyed barrier a real cross-talk bug.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { Binding, Curve, ModulationSnapshot } from "../types/ipc";

const curvesOnServer: Curve[] = [];
const bindingsOnServer: Binding[] = [];
const setPayloads: Curve[] = [];
let pending: (() => void)[] = [];
let holdReplies = false;

/** What the backend does: sort by tick, collapse duplicate ticks last-wins,
 * mint an empty id. Modelled here so a test can tell the backend's
 * authoritative answer apart from the payload the client sent. */
function normalize(curve: Curve): Curve {
  const byTick = new Map<number, number>();
  for (const p of curve.points) byTick.set(p.tick, p.value);
  return {
    ...curve,
    id: curve.id || "minted-1",
    points: [...byTick.entries()]
      .sort((a, b) => a[0] - b[0])
      .map(([tick, value]) => ({ tick, value })),
  };
}

function snapshot(): ModulationSnapshot {
  return {
    curves: curvesOnServer.map((c) => structuredClone(c)),
    bindings: bindingsOnServer.map((b) => structuredClone(b)),
    automationClips: [],
  };
}

const modulationSetCurve = vi.fn((curve: Curve) => {
  setPayloads.push(structuredClone(curve));
  const apply = () => {
    const norm = normalize(curve);
    const i = curvesOnServer.findIndex((c) => c.id === norm.id);
    if (i >= 0) curvesOnServer[i] = structuredClone(norm);
    else curvesOnServer.push(structuredClone(norm));
    return snapshot();
  };
  if (!holdReplies) return Promise.resolve(apply());
  return new Promise<ModulationSnapshot>((res) => {
    pending.push(() => res(apply()));
  });
});

vi.mock("../tauri", () => ({
  backend: {
    mode: "tauri",
    on: () => () => {},
    modulationGet: vi.fn(() => Promise.resolve(snapshot())),
    modulationSetCurve,
    modulationSetBinding: vi.fn((b: Binding) => {
      const i = bindingsOnServer.findIndex((x) => x.id === b.id);
      if (i >= 0) bindingsOnServer[i] = structuredClone(b);
      else bindingsOnServer.push(structuredClone(b));
      return Promise.resolve(snapshot());
    }),
  },
}));

const { modulation } = await import("../state/modulation.svelte");

const P0 = { tick: 0, value: 1 };
const P480 = { tick: 480, value: 0.4 };

function seededCurve(): Curve {
  return { id: "cur-a", name: "gain", points: [{ ...P0 }] };
}

function seededBinding(): Binding {
  return {
    id: "bnd-a",
    source: { kind: "curve", curveId: "cur-a" },
    target: { kind: "trackParam", trackId: "t-1", param: "gain" },
    mode: "multiply",
    depth: 1,
  };
}

async function flush() {
  for (let i = 0; i < 6; i++) {
    const due = pending;
    pending = [];
    for (const r of due) r();
    await Promise.resolve();
    await Promise.resolve();
  }
}

beforeEach(() => {
  vi.clearAllMocks();
  curvesOnServer.length = 0;
  bindingsOnServer.length = 0;
  setPayloads.length = 0;
  pending = [];
  holdReplies = false;
  modulation.curves = [];
  modulation.bindings = [];
  modulation.automationClips = [];
  modulation.visible = new Map();
});

describe("modulation lane gesture sequencing", () => {
  it("a drag previews locally and commits exactly once on pointerup", async () => {
    curvesOnServer.push(seededCurve());
    bindingsOnServer.push(seededBinding());
    await modulation.reload();
    expect(modulation.curves[0].points).toHaveLength(1);

    modulation.preview("cur-a", [{ ...P0 }, { ...P480 }]);
    expect(modulationSetCurve).not.toHaveBeenCalled();
    expect(modulation.curves[0].points).toEqual([P0, P480]);

    await modulation.commitLatest("bnd-a");
    expect(modulationSetCurve).toHaveBeenCalledTimes(1);
    expect(setPayloads[0].points).toEqual([P0, P480]);
  });

  it("a click-insert released inside the round-trip window keeps the new point", async () => {
    curvesOnServer.push(seededCurve());
    bindingsOnServer.push(seededBinding());
    await modulation.reload();
    expect(modulation.curves[0].points).toHaveLength(1);

    holdReplies = true;
    // pointerdown: insert at tick 480 and put it on the wire (not awaited,
    // exactly like the handler).
    const inserted = { ...modulation.curves[0], points: [{ ...P0 }, { ...P480 }] };
    void modulation.commitInGesture("bnd-a", inserted);
    // pointerup arrives BEFORE that reply — the whole bug.
    const closing = modulation.commitLatest("bnd-a");

    await flush();
    await closing;

    expect(curvesOnServer[0].points).toEqual([P0, P480]);
    expect(modulation.curves[0].points).toEqual([P0, P480]);
    // and the closing commit must not have sent the stale one-point set
    expect(setPayloads.map((c) => c.points.length)).not.toContain(1);
  });

  it("the closing commit sends the backend's NORMALIZED curve, not the client's payload", async () => {
    curvesOnServer.push(seededCurve());
    bindingsOnServer.push(seededBinding());
    await modulation.reload();
    holdReplies = true;
    void modulation.commitInGesture("bnd-a", {
      ...modulation.curves[0],
      points: [{ ...P480 }, { tick: 480, value: 0.9 }, { ...P0 }],
    });
    const closing = modulation.commitLatest("bnd-a");
    await flush();
    await closing;

    expect(modulationSetCurve).toHaveBeenCalledTimes(2);
    expect(setPayloads[1].points).toEqual([P0, { tick: 480, value: 0.9 }]);
    expect(setPayloads[1].id).toBe("cur-a");
  });

  it("a curve minted by the insert is the one the closing commit updates", async () => {
    bindingsOnServer.push({
      id: "bnd-new",
      source: { kind: "curve", curveId: "minted-1" },
      target: { kind: "trackParam", trackId: "t-9", param: "gain" },
      mode: "multiply",
      depth: 1,
    });
    holdReplies = true;
    void modulation.commitInGesture("bnd-new", {
      id: "",
      name: "gain",
      points: [{ ...P480 }],
    });
    const closing = modulation.commitLatest("bnd-new");
    await flush();
    await closing;

    expect(curvesOnServer).toHaveLength(1);
    expect(curvesOnServer[0].points).toEqual([P480]);
    expect(curvesOnServer[0].id).toBe("minted-1");
  });

  it("two lanes on one track keep independent commit barriers", async () => {
    const gain: Curve = { id: "cur-gain", name: "gain", points: [{ ...P0 }] };
    const pan: Curve = { id: "cur-pan", name: "pan", points: [{ tick: 0, value: 0.5 }] };
    curvesOnServer.push(gain, pan);
    bindingsOnServer.push(
      {
        id: "bnd-gain",
        source: { kind: "curve", curveId: "cur-gain" },
        target: { kind: "trackParam", trackId: "t-1", param: "gain" },
        mode: "multiply",
        depth: 1,
      },
      {
        id: "bnd-pan",
        source: { kind: "curve", curveId: "cur-pan" },
        target: { kind: "trackParam", trackId: "t-1", param: "pan" },
        mode: "absolute",
        depth: 1,
      },
    );
    await modulation.reload();

    holdReplies = true;
    void modulation.commitInGesture("bnd-gain", {
      ...gain,
      points: [{ ...P0 }, { ...P480 }],
    });
    void modulation.commitInGesture("bnd-pan", {
      ...pan,
      points: [{ tick: 0, value: 0.25 }],
    });
    const closeGain = modulation.commitLatest("bnd-gain");
    const closePan = modulation.commitLatest("bnd-pan");
    await flush();
    await closeGain;
    await closePan;

    expect(curvesOnServer.find((c) => c.id === "cur-gain")!.points).toEqual([P0, P480]);
    expect(curvesOnServer.find((c) => c.id === "cur-pan")!.points).toEqual([
      { tick: 0, value: 0.25 },
    ]);
    // neither closing commit echoed the other lane's pre-insert set
    const gainPayloads = setPayloads.filter((c) => c.id === "cur-gain" || c.name === "gain");
    const panPayloads = setPayloads.filter((c) => c.id === "cur-pan" || c.name === "pan");
    expect(gainPayloads.some((c) => c.points.length === 1 && c.id === "cur-gain")).toBe(false);
    expect(panPayloads.some((c) => c.points[0]?.value === 0.5 && c.points.length === 1)).toBe(
      false,
    );
  });
});
