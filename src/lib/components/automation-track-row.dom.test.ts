// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import type { AutomationClip, Curve, TrackState } from "../types/ipc";

/**
 * `AutomationTrackRow`'s pointer/gesture handlers, mounted for real. The
 * Track D handoff (`docs/PHASE4-PLAN.md`) names this exact class of code —
 * ordering of async effects inside a `.svelte` event handler — as the
 * uncovered gap where both of that track's real frontend bugs lived,
 * because until now nothing could mount a component to exercise it.
 */

let curvesOnServer: Curve[] = [];
let clipsOnServer: AutomationClip[] = [];

function snapshot() {
  return {
    curves: curvesOnServer.map((c) => structuredClone(c)),
    bindings: [],
    automationClips: clipsOnServer.map((c) => structuredClone(c)),
  };
}

const modulationSetCurve = vi.fn((curve: Curve) => {
  const i = curvesOnServer.findIndex((c) => c.id === curve.id);
  if (i >= 0) curvesOnServer[i] = structuredClone(curve);
  else curvesOnServer.push(structuredClone(curve));
  return Promise.resolve(snapshot());
});

const automationClipSet = vi.fn((clip: AutomationClip, remove?: boolean) => {
  if (remove) clipsOnServer = clipsOnServer.filter((c) => c.id !== clip.id);
  else {
    const i = clipsOnServer.findIndex((c) => c.id === clip.id);
    if (i >= 0) clipsOnServer[i] = structuredClone(clip);
    else clipsOnServer.push(structuredClone(clip));
  }
  return Promise.resolve(snapshot());
});

const gestureBegin = vi.fn((label: string) => Promise.resolve(`gesture-${label}`));
const gestureEnd = vi.fn((_id: string) => Promise.resolve());

vi.mock("../tauri", () => ({
  backend: {
    mode: "tauri",
    on: () => () => {},
    gestureBegin,
    gestureEnd,
    modulationSetCurve,
    automationClipSet,
  },
}));

const { default: AutomationTrackRow } = await import("./AutomationTrackRow.svelte");
const { modulation } = await import("../state/modulation.svelte");
const { midi } = await import("../state/midi.svelte");

// 120 BPM @ 48 kHz, identity-ish (25 samples/tick). See this plan's Global
// Constraints for why an unseeded `sectionTable` silently hides every clip.
const SECTION_TABLE = [{ startTick: 0, startSample: 0, startBeat: 0, startBar: 0, period: 254_016_000 }];

function seedTrack(): TrackState {
  return {
    id: "t-1",
    name: "Auto",
    kind: "automation",
    gainDb: 0,
    pan: 0,
    muted: false,
    soloed: false,
    armed: false,
    color: "#38bdf8",
  };
}

function seedCurve(): Curve {
  return { id: "curve-1", name: "Cutoff", points: [{ tick: 0, value: 1 }] };
}

function seedClip(): AutomationClip {
  return {
    id: "clip-1",
    trackId: "t-1",
    curveId: "curve-1",
    timelineStartTicks: 0,
    lengthTicks: 1920,
    contentLengthTicks: 1920,
  };
}

function seed() {
  midi.sectionTable = SECTION_TABLE;
  modulation.curves = [seedCurve()];
  modulation.automationClips = [seedClip()];
}

afterEach(() => {
  cleanup();
  curvesOnServer = [];
  clipsOnServer = [];
  vi.clearAllMocks();
  modulation.curves = [];
  modulation.automationClips = [];
  midi.sectionTable = [];
});

describe("AutomationTrackRow", () => {
  it("a plain click selects the clip and shows the delete button", async () => {
    seed();
    render(AutomationTrackRow, { props: { track: seedTrack() } });

    const clipEl = screen.getByRole("button", { name: /automation clip/i });
    expect(screen.queryByRole("button", { name: /delete automation clip/i })).toBeNull();

    await fireEvent.pointerDown(clipEl, { clientX: 50, clientY: 5, button: 0 });
    await fireEvent.pointerUp(clipEl, { clientX: 50, clientY: 5, button: 0 });

    expect(screen.getByRole("button", { name: /delete automation clip/i })).toBeTruthy();
    expect(automationClipSet).not.toHaveBeenCalled();
    expect(gestureBegin).not.toHaveBeenCalled();
  });

  it("clicking the delete button removes the clip", async () => {
    seed();
    render(AutomationTrackRow, { props: { track: seedTrack() } });

    const clipEl = screen.getByRole("button", { name: /automation clip/i });
    await fireEvent.pointerDown(clipEl, { clientX: 50, clientY: 5, button: 0 });
    await fireEvent.pointerUp(clipEl, { clientX: 50, clientY: 5, button: 0 });

    const delBtn = screen.getByRole("button", { name: /delete automation clip/i });
    await fireEvent.click(delBtn);

    expect(automationClipSet).toHaveBeenCalledTimes(1);
    expect(automationClipSet).toHaveBeenCalledWith(expect.objectContaining({ id: "clip-1" }), true);
  });
});
