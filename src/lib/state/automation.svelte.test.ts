/**
 * Automation lane store: a thin mirror of the backend's `automation[]`
 * (ADR 0006 — no authoritative state here, no tick math here). `preview`
 * is the drag-time local patch; `commit` is the one invoke per gesture.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { AutomationLane } from "../types/ipc";

const lanesOnServer: AutomationLane[] = [];
const automationGet = vi.fn(() => Promise.resolve([...lanesOnServer]));
const automationSet = vi.fn((lane: AutomationLane) => {
  const i = lanesOnServer.findIndex((l) => l.id === lane.id);
  if (lane.points.length === 0) {
    if (i >= 0) lanesOnServer.splice(i, 1);
  } else if (i >= 0) lanesOnServer[i] = lane;
  else lanesOnServer.push(lane);
  return Promise.resolve([...lanesOnServer]);
});

vi.mock("../tauri", () => ({
  backend: { mode: "tauri", on: () => () => {}, automationGet, automationSet },
}));

const { automation, TRACK_PARAM_GAIN, trackTarget } = await import("./automation.svelte");

const lane = (id: string, trackId: string): AutomationLane => ({
  id,
  targetNode: trackTarget(trackId),
  paramId: TRACK_PARAM_GAIN,
  points: [
    { tick: 0, value: 1 },
    { tick: 3840, value: 0 },
  ],
});

beforeEach(() => {
  vi.clearAllMocks();
  lanesOnServer.length = 0;
  automation.lanes = [];
  automation.visible = new Set();
});

describe("automation store", () => {
  it("reload pulls the backend list", async () => {
    lanesOnServer.push(lane("a", "t-1"));
    await automation.reload();
    expect(automation.lanes).toHaveLength(1);
    expect(automation.gainLaneFor("t-1")?.id).toBe("a");
    expect(automation.gainLaneFor("t-2")).toBeUndefined();
  });

  it("preview patches locally without invoking", () => {
    automation.lanes = [lane("a", "t-1")];
    automation.preview("a", [{ tick: 0, value: 0.5 }]);
    expect(automation.gainLaneFor("t-1")!.points).toEqual([{ tick: 0, value: 0.5 }]);
    expect(automationSet).not.toHaveBeenCalled();
  });

  it("commit invokes once and adopts the backend's authoritative list", async () => {
    await automation.commit(lane("a", "t-1"));
    expect(automationSet).toHaveBeenCalledTimes(1);
    expect(automation.lanes).toHaveLength(1);
  });

  it("commit with no points deletes the lane", async () => {
    await automation.commit(lane("a", "t-1"));
    await automation.commit({ ...lane("a", "t-1"), points: [] });
    expect(automation.lanes).toHaveLength(0);
  });

  it("reload is silent when the backend has no automation commands (demo)", async () => {
    const { backend } = await import("../tauri");
    const b = backend as unknown as Record<string, unknown>;
    const saved = b.automationGet;
    delete b.automationGet;
    await automation.reload();
    expect(automation.lanes).toEqual([]);
    b.automationGet = saved;
  });

  it("automatePluginParam creates a flat lane, and toggling again deletes it", async () => {
    await automation.automatePluginParam("inst-1", 7, 0.25);
    expect(automation.pluginLaneFor("inst-1", 7)?.points).toEqual([{ tick: 0, value: 0.25 }]);
    await automation.automatePluginParam("inst-1", 7, 0.25);
    expect(automation.pluginLaneFor("inst-1", 7)).toBeUndefined();
  });

  it("toggleVisible flips per-track overlay visibility", () => {
    expect(automation.isVisible("t-1")).toBe(false);
    automation.toggleVisible("t-1");
    expect(automation.isVisible("t-1")).toBe(true);
    automation.toggleVisible("t-1");
    expect(automation.isVisible("t-1")).toBe(false);
  });
});
