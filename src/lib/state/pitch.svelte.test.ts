/**
 * The pitch frame bus. Like the meter bus it is deliberately not reactive,
 * so these tests drive `ingestBatch` directly — no backend, no rAF.
 */
import { describe, it, expect, beforeEach } from "vitest";
import {
  ingestBatch,
  recentFrames,
  latestVoiced,
  framesBetween,
  resetPitchBus,
  pitchMode,
  RING_CAPACITY,
  pitchCacheRevision,
  invalidatePitchCache,
} from "./pitch.svelte";
import type { PitchFrame } from "../types/ipc";

const f = (sample: number, midi: number, voiced = true): PitchFrame => ({
  sample,
  midi,
  hz: 440 * 2 ** ((midi - 69) / 12),
  clarity: 0.9,
  rms: 0.2,
  voiced,
});

describe("pitch frame bus", () => {
  beforeEach(() => resetPitchBus());

  it("keeps frames in arrival order", () => {
    ingestBatch({ frames: [f(0, 57), f(480, 57)], deviceRate: 48000, listening: true, rehearseHold: false });
    expect(recentFrames().map((x) => x.sample)).toEqual([0, 480]);
  });

  it("bounds the ring so a long session cannot grow without limit", () => {
    for (let i = 0; i < 5000; i++) {
      ingestBatch({ frames: [f(i * 480, 57)], deviceRate: 48000, listening: true, rehearseHold: false });
    }
    expect(recentFrames().length).toBeLessThanOrEqual(3000);
    // The newest frame must survive; the ring drops the oldest.
    expect(recentFrames()[recentFrames().length - 1].sample).toBe(4999 * 480);
  });

  it("stays in order across the wrap, and drops exactly the oldest", () => {
    // A ring read that forgets the wrap returns the tail before the head:
    // the trail then draws a jump backwards in time once every 30 seconds.
    const total = RING_CAPACITY + 17;
    for (let i = 0; i < total; i++) {
      ingestBatch({ frames: [f(i * 480, 57)], deviceRate: 48000, listening: true, rehearseHold: false });
    }
    const samples = recentFrames().map((x) => x.sample);
    expect(samples.length).toBe(RING_CAPACITY);
    expect(samples[0]).toBe(17 * 480);
    expect(samples).toEqual([...samples].sort((a, b) => a - b));
  });

  it("keeps the newest frames when one batch is larger than the ring", () => {
    // Defensive: a 60 Hz batch holds ~6 frames, but a stall that backs the
    // worker up must not corrupt the cursor.
    const frames = Array.from({ length: RING_CAPACITY + 40 }, (_, i) => f(i * 480, 57));
    ingestBatch({ frames, deviceRate: 48000, listening: true, rehearseHold: false });
    const samples = recentFrames().map((x) => x.sample);
    expect(samples.length).toBe(RING_CAPACITY);
    expect(samples[samples.length - 1]).toBe((RING_CAPACITY + 39) * 480);
    expect(samples[0]).toBe(40 * 480);
  });

  it("latestVoiced skips unvoiced frames", () => {
    ingestBatch({ frames: [f(0, 57), f(480, 0, false)], deviceRate: 48000, listening: true, rehearseHold: false });
    expect(latestVoiced()?.sample).toBe(0);
  });

  it("latestVoiced is null when nothing has been voiced yet", () => {
    ingestBatch({ frames: [f(0, 0, false)], deviceRate: 48000, listening: true, rehearseHold: false });
    expect(latestVoiced()).toBeNull();
  });

  it("framesBetween returns the half-open sample window", () => {
    ingestBatch({
      frames: [f(0, 57), f(480, 58), f(960, 59)],
      deviceRate: 48000,
      listening: true,
      rehearseHold: false,
    });
    expect(framesBetween(480, 960).map((x) => x.sample)).toEqual([480]);
  });

  it("framesBetween spans the wrap without losing the older half", () => {
    for (let i = 0; i < RING_CAPACITY + 100; i++) {
      ingestBatch({ frames: [f(i * 480, 57)], deviceRate: 48000, listening: true, rehearseHold: false });
    }
    const first = 100 * 480; // oldest frame still in the ring
    const window = framesBetween(first, first + 3 * 480);
    expect(window.map((x) => x.sample)).toEqual([first, first + 480, first + 2 * 480]);
  });

  it("mirrors mode flags from the batch", () => {
    ingestBatch({ frames: [], deviceRate: 48000, listening: true, rehearseHold: true });
    expect(pitchMode.rehearseHold).toBe(true);
    expect(pitchMode.listening).toBe(true);
    expect(pitchMode.deviceRate).toBe(48000);
  });

  it("resetPitchBus clears frames and flags", () => {
    ingestBatch({ frames: [f(0, 57)], deviceRate: 48000, listening: true, rehearseHold: true });
    resetPitchBus();
    expect(recentFrames()).toEqual([]);
    expect(latestVoiced()).toBeNull();
    expect(pitchMode.listening).toBe(false);
    expect(pitchMode.rehearseHold).toBe(false);
  });

  it("invalidatePitchCache increments revision per clip", () => {
    expect(pitchCacheRevision["clip-1"]).toBeUndefined();
    invalidatePitchCache("clip-1");
    expect(pitchCacheRevision["clip-1"]).toBe(1);
    invalidatePitchCache("clip-1");
    expect(pitchCacheRevision["clip-1"]).toBe(2);
    invalidatePitchCache("clip-2");
    expect(pitchCacheRevision["clip-2"]).toBe(1);
  });
});
