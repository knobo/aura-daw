/**
 * The call site, not the store: `paramFollow` is only useful if the meter
 * stream actually hands it every frame's read-back. Track D's review I-4 was
 * exactly this class of gap — deleting the engine's `drive_param_automation()`
 * call left every other test green, because nothing covered the wiring.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { MeterFrame } from "../types/ipc";

let onFrame: ((frame: MeterFrame) => void) | null = null;

vi.mock("../tauri", () => ({
  backend: {
    subscribeMeters: (cb: (frame: MeterFrame) => void) => {
      onFrame = cb;
      return Promise.resolve(() => {
        onFrame = null;
      });
    },
  },
}));

const { startMeterStream, stopMeterStream } = await import("./meters.svelte");
const { paramFollow } = await import("./param-follow.svelte");

function meter() {
  return { trackId: "master", peakL: 0, peakR: 0, rmsL: 0, rmsR: 0, clipped: false };
}

function frame(drivenParams: MeterFrame["drivenParams"]): MeterFrame {
  return { seq: 0, positionSamples: 0, tracks: [], master: meter(), drivenParams };
}

beforeEach(() => {
  stopMeterStream();
  paramFollow.reset();
});

describe("the meter stream feeds the driven-param read-back", () => {
  it("hands each frame's drivenParams to paramFollow", async () => {
    await startMeterStream();
    onFrame?.(frame([{ instanceId: "i1", index: 7, value: 0.75 }]));
    expect(paramFollow.valueFor("i1", 7)).toBe(0.75);

    onFrame?.(frame([{ instanceId: "i1", index: 7, value: 0.25 }]));
    expect(paramFollow.valueFor("i1", 7)).toBe(0.25);
  });

  it("clears the read-back when the stream stops", async () => {
    await startMeterStream();
    onFrame?.(frame([{ instanceId: "i1", index: 7, value: 0.75 }]));
    stopMeterStream();
    expect(paramFollow.active).toBe(false);
  });

  it("survives a frame from a backend that has no drivenParams field", async () => {
    await startMeterStream();
    onFrame?.(frame(undefined));
    expect(paramFollow.active).toBe(false);
  });
});
