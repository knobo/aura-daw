/**
 * Escape = stop everything that is making sound.
 *
 * The three sources are independent, and the launch overlay is the one that
 * surprises people: a previewed clip is rendered exclusively while the
 * transport is stopped, so stopping the transport does NOT silence it.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { TransportState } from "../types/ipc";

const calls: string[] = [];

const STOPPED: TransportState = {
  state: "stopped",
  positionSamples: 48_000,
  sampleRate: 48_000,
  tempoBpm: 120,
  loopEnabled: false,
  loopStartSamples: 0,
  loopEndSamples: 0,
  songEndSamples: 96_000,
  stopAtEnd: true,
};

vi.mock("../tauri", () => ({
  backend: {
    mode: "tauri",
    on: () => () => {},
    transportStop: () => {
      calls.push("transportStop");
      return Promise.resolve(STOPPED);
    },
    transportSeek: (pos: number) => {
      calls.push(`transportSeek:${pos}`);
      return Promise.resolve(STOPPED);
    },
    launchStop: () => {
      calls.push("launchStop");
      return Promise.resolve();
    },
    libraryAuditionStop: () => {
      calls.push("auditionStop");
      return Promise.resolve();
    },
  },
}));

const { stopAllSound } = await import("./stop-all");
const { transport } = await import("./transport.svelte");
const { launch } = await import("./launch.svelte");
const { surface } = await import("./surface.svelte");
const { audition } = await import("./audition.svelte");

beforeEach(() => {
  calls.length = 0;
  transport.snap = { ...STOPPED };
  launch.maps = [
    {
      ...launch.maps[0],
      bindings: [{ id: "b1", name: "Verse", target: { kind: "clip", clipId: "c1" } }],
    },
  ] as unknown as typeof launch.maps;
  launch.activeMapId = launch.maps[0].id;
  launch.overlay = null;
});

describe("stopAllSound", () => {
  it("cuts the launch overlay even when the transport is already stopped", async () => {
    launch.overlay = { id: "b1", name: "Verse" };
    await stopAllSound();
    expect(calls).toContain("launchStop");
    expect(launch.overlay).toBe(null);
    // Nothing was rolling, so the transport is left alone — Escape twice in a
    // row must not turn into a seek.
    expect(calls).not.toContain("transportStop");
    expect(calls.some((c) => c.startsWith("transportSeek"))).toBe(false);
  });

  it("pauses a rolling transport without moving the playhead", async () => {
    transport.snap = { ...STOPPED, state: "playing", positionSamples: 48_000 };
    await stopAllSound();
    expect(calls).toContain("transportStop");
    expect(calls.some((c) => c.startsWith("transportSeek"))).toBe(false);
    expect(transport.snap.positionSamples).toBe(48_000);
  });

  it("releases the audition preview stream", async () => {
    const stop = vi.spyOn(audition, "stop");
    await stopAllSound();
    expect(stop).toHaveBeenCalledTimes(1);
    stop.mockRestore();
  });

  it("survives a leg that throws", async () => {
    const stop = vi.spyOn(audition, "stop").mockRejectedValue(new Error("nope"));
    launch.overlay = { id: "b1", name: "Verse" };
    await expect(stopAllSound()).resolves.toBeUndefined();
    expect(calls).toContain("launchStop");
    stop.mockRestore();
  });
});

describe("a toggle pad's second press", () => {
  it("cuts the clip it is holding", async () => {
    launch.overlay = { id: "b1", name: "Verse" };
    await surface.stopClip("c1");
    expect(calls).toContain("launchStop");
  });

  it("leaves another clip's overlay alone", async () => {
    launch.overlay = { id: "b1", name: "Verse" };
    await surface.stopClip("c-other");
    expect(calls).not.toContain("launchStop");
  });

  it("does not fire a stop for a clip that already ended", async () => {
    // The overlay is gone, so the pad is not lit and its next press must
    // FIRE the clip again rather than stop a silence.
    launch.overlay = null;
    await surface.stopClip("c1");
    expect(calls).not.toContain("launchStop");
  });

  it("cuts a scene pad's own binding, not somebody else's", async () => {
    launch.overlay = { id: "b1", name: "Verse" };
    await surface.stopBinding("b-other");
    expect(calls).not.toContain("launchStop");
    await surface.stopBinding("b1");
    expect(calls).toContain("launchStop");
  });
});
