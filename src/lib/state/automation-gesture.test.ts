/**
 * The lane overlay's click-insert race (whole-track review, blocker).
 *
 * `AutomationLaneView`'s pointerdown inserts a point and commits WITHOUT
 * awaiting; its pointerup then re-reads the store and commits that. The store
 * is only written when `automation_set` RESOLVES, so a pointerup landing
 * inside the insert's round-trip window reads the PRE-insert lane and commits
 * it back over the fresh point — last arrival wins and the point vanishes.
 * Both commits fold into one gesture, so no undo entry reveals the loss, and a
 * normal 50-150 ms click usually beats the round trip, which makes it
 * intermittent.
 *
 * This exercises the two store methods the handlers delegate to
 * (`commitInGesture` = pointerdown, `commitLatest` = pointerup), because the
 * repo has no DOM test environment to mount the component in.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { AutomationLane } from "../types/ipc";

/** Lanes as the backend holds them, plus a hold-the-reply switch so a test
 * can keep a commit "on the wire" exactly like a real IPC round trip. */
const lanesOnServer: AutomationLane[] = [];
const setPayloads: AutomationLane[] = [];
let pending: (() => void)[] = [];
let holdReplies = false;

/** What `plugins::automation::normalize_lane` does: sort by tick, collapse
 * duplicate ticks last-wins. Modelled here so a test can tell the backend's
 * authoritative answer apart from the payload the client sent. */
function normalize(lane: AutomationLane): AutomationLane {
  const byTick = new Map<number, number>();
  for (const p of lane.points) byTick.set(p.tick, p.value);
  return {
    ...lane,
    id: lane.id || "minted-1",
    points: [...byTick.entries()]
      .sort((a, b) => a[0] - b[0])
      .map(([tick, value]) => ({ tick, value })),
  };
}

const automationSet = vi.fn((lane: AutomationLane) => {
  setPayloads.push(structuredClone(lane));
  const apply = () => {
    const norm = normalize(lane);
    const i = lanesOnServer.findIndex((l) => l.id === norm.id);
    if (norm.points.length === 0) {
      if (i >= 0) lanesOnServer.splice(i, 1);
    } else if (i >= 0) lanesOnServer[i] = structuredClone(norm);
    else lanesOnServer.push(structuredClone(norm));
    return lanesOnServer.map((l) => structuredClone(l));
  };
  if (!holdReplies) return Promise.resolve(apply());
  return new Promise<AutomationLane[]>((res) => {
    pending.push(() => res(apply()));
  });
});

vi.mock("../tauri", () => ({
  backend: {
    mode: "tauri",
    on: () => () => {},
    automationGet: vi.fn(() => Promise.resolve([...lanesOnServer])),
    automationSet,
  },
}));

const { automation, TRACK_PARAM_GAIN, trackTarget } = await import("./automation.svelte");

const P0 = { tick: 0, value: 1 };
const P480 = { tick: 480, value: 0.4 };

function seededLane(): AutomationLane {
  return {
    id: "lane-a",
    targetNode: trackTarget("t-1"),
    paramId: TRACK_PARAM_GAIN,
    points: [{ ...P0 }],
  };
}

/** Release every reply the backend is holding, then let the chained
 * continuations run. */
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
  lanesOnServer.length = 0;
  setPayloads.length = 0;
  pending = [];
  holdReplies = false;
  automation.lanes = [];
  automation.visible = new Set();
});

describe("automation lane gesture sequencing", () => {
  it("a click-insert released inside the round-trip window keeps the new point", async () => {
    lanesOnServer.push(seededLane());
    await automation.reload();
    expect(automation.gainLaneFor("t-1")!.points).toHaveLength(1);

    holdReplies = true;
    // pointerdown: insert at tick 480 and put it on the wire (not awaited,
    // exactly like the handler).
    const inserted = { ...automation.gainLaneFor("t-1")!, points: [{ ...P0 }, { ...P480 }] };
    void automation.commitInGesture(inserted);
    // pointerup arrives BEFORE that reply — the whole bug.
    const closing = automation.commitLatest("t-1");

    await flush();
    await closing;

    expect(lanesOnServer[0].points).toEqual([P0, P480]);
    expect(automation.gainLaneFor("t-1")!.points).toEqual([P0, P480]);
    // and the closing commit must not have sent the stale one-point set
    expect(setPayloads.map((l) => l.points.length)).not.toContain(1);
  });

  it("the closing commit sends the backend's NORMALIZED lane, not the client's payload", async () => {
    // The backend is authoritative (sorts by tick, collapses duplicates), so
    // the closing commit must echo what came back — not re-send the raw
    // payload the insert happened to build.
    lanesOnServer.push(seededLane());
    await automation.reload();
    holdReplies = true;
    // deliberately out of order, with a duplicate tick the backend collapses
    void automation.commitInGesture({
      ...automation.gainLaneFor("t-1")!,
      points: [{ ...P480 }, { tick: 480, value: 0.9 }, { ...P0 }],
    });
    const closing = automation.commitLatest("t-1");
    await flush();
    await closing;

    expect(automationSet).toHaveBeenCalledTimes(2);
    expect(setPayloads[1].points).toEqual([P0, { tick: 480, value: 0.9 }]);
    expect(setPayloads[1].id).toBe("lane-a");
  });

  it("a lane minted by the insert is the one the closing commit updates", async () => {
    // First point on a track: the lane has no id yet, so the insert MUST
    // reach the backend to be minted — and the closing commit must address
    // the minted lane, not re-mint a second one.
    holdReplies = true;
    void automation.commitInGesture({
      id: "",
      targetNode: trackTarget("t-9"),
      paramId: TRACK_PARAM_GAIN,
      points: [{ ...P480 }],
    });
    const closing = automation.commitLatest("t-9");
    await flush();
    await closing;

    expect(lanesOnServer).toHaveLength(1);
    expect(lanesOnServer[0].points).toEqual([P480]);
  });
});
