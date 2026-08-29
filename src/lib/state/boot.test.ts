/**
 * Boot store: phase transitions driven by App.svelte (`setPhase`/`finish`/
 * `fail`) and by the two backend progress events `wire()` subscribes to.
 * The two feeds are deliberately independent — a backend that never fires
 * either event, and demo mode which never fires them at all, must still
 * reach "ready" through `finish()` alone.
 *
 * "ready" is computed (`chainDone && !mediaInFlight`, in `settle()`), not
 * simply "finish() was called" — `open_project` returns before the engine
 * finishes decoding audio, so a `project://media-progress` event can land
 * either before or after `finish()`. The tests below exercise both orders,
 * plus the documented choice for what happens to a media event that lands
 * long after a full boot cycle has already reached "ready".
 */

import { beforeEach, describe, expect, it, vi } from "vitest";
import type { AuraEventMap } from "../types/ipc";

type Handler<K extends keyof AuraEventMap> = (payload: AuraEventMap[K]) => void;

const handlers = new Map<keyof AuraEventMap, Handler<never>>();
const backendOn = vi.fn((event: keyof AuraEventMap, cb: Handler<never>) => {
  handlers.set(event, cb);
  return () => {
    handlers.delete(event);
  };
});

vi.mock("../tauri", () => ({
  backend: {
    mode: "tauri" as const,
    on: (event: keyof AuraEventMap, cb: Handler<never>) => backendOn(event, cb),
  },
}));

const { boot } = await import("./boot.svelte");

function fireOpenProgress(payload: AuraEventMap["project://open-progress"]) {
  handlers.get("project://open-progress")?.(payload as never);
}

function fireMediaProgress(payload: AuraEventMap["project://media-progress"]) {
  handlers.get("project://media-progress")?.(payload as never);
}

beforeEach(() => {
  boot.reset();
  handlers.clear();
  backendOn.mockClear();
  vi.useRealTimers();
});

describe("boot store", () => {
  it("starts in the starting phase with its default label", () => {
    expect(boot.phase).toBe("starting");
    expect(boot.label).toBe("Starting AURA…");
    expect(boot.progress).toBeNull();
    expect(boot.error).toBeNull();
  });

  it("setPhase moves through the App.svelte-driven phases with default labels", () => {
    boot.setPhase("stores");
    expect(boot.phase).toBe("stores");
    expect(boot.label).toBe("Connecting to the audio engine…");

    boot.setPhase("project");
    expect(boot.phase).toBe("project");
    expect(boot.label).toBe("Opening the last project…");
  });

  it("setPhase accepts an explicit label override", () => {
    boot.setPhase("project", "Reopening Wednesday Session");
    expect(boot.label).toBe("Reopening Wednesday Session");
  });

  it("finish() reaches ready regardless of whether any progress event ever fired", () => {
    // No wire(), no events — mirrors demo mode and an older backend alike.
    boot.setPhase("stores");
    boot.finish();
    expect(boot.phase).toBe("ready");
    expect(boot.label).toBe("Ready");
    expect(boot.progress).toBe(1);
    expect(boot.error).toBeNull();
  });

  it("fail() records the error and leaves the phase failed", () => {
    boot.setPhase("project");
    boot.fail(new Error("could not reach the engine"));
    expect(boot.phase).toBe("failed");
    expect(boot.error).toBe("could not reach the engine");
  });

  it("fail() stringifies a non-Error thrown value", () => {
    boot.fail("boom");
    expect(boot.phase).toBe("failed");
    expect(boot.error).toBe("boom");
  });

  it("wire() maps project://open-progress payloads to a label and fraction", () => {
    const stop = boot.wire();
    fireOpenProgress({
      step: "plugins",
      index: 2,
      total: 4,
      label: "Loading plugins — 2 of 4",
      detail: "reverb.vst3",
    });
    expect(boot.phase).toBe("project");
    expect(boot.label).toBe("Loading plugins — 2 of 4");
    expect(boot.detail).toBe("reverb.vst3");
    expect(boot.progress).toBeCloseTo(0.5);
    stop();
  });

  it("wire() falls back to the step's default wording when label is empty", () => {
    boot.wire();
    fireOpenProgress({
      step: "midiOut",
      index: 0,
      total: 0,
      label: "",
      detail: null,
    });
    expect(boot.label).toBe("Connecting MIDI outputs");
    expect(boot.progress).toBeNull(); // total 0 -> no fraction
  });

  it("wire() maps project://media-progress payloads to a label and fraction", () => {
    boot.wire();
    fireMediaProgress({
      loaded: 4,
      total: 19,
      name: "kick.wav",
      phase: "decode",
      done: false,
    });
    expect(boot.phase).toBe("media");
    expect(boot.label).toBe("Loading audio — 4 of 19 files");
    expect(boot.detail).toBe("kick.wav");
    expect(boot.progress).toBeCloseTo(4 / 19);

    fireMediaProgress({
      loaded: 19,
      total: 19,
      name: null,
      phase: "peaks",
      done: false,
    });
    expect(boot.label).toBe("Building waveforms — 19 of 19 files");
  });

  it("media-progress with no total yet falls back to the bare verb", () => {
    boot.wire();
    fireMediaProgress({ loaded: 0, total: 0, name: null, phase: "decode", done: false });
    expect(boot.label).toBe("Loading audio");
    expect(boot.progress).toBeNull();
  });

  it("does not settle to ready while finish() runs but a media decode is still in flight", () => {
    // open_project's promise can resolve before the engine's decode pass —
    // simulate the decode starting first this time.
    boot.wire();
    fireMediaProgress({ loaded: 1, total: 3, name: "kick.wav", phase: "decode", done: false });
    boot.finish();
    expect(boot.phase).toBe("media");
    expect(boot.label).toBe("Loading audio — 1 of 3 files");

    fireMediaProgress({ loaded: 3, total: 3, name: null, phase: "decode", done: true });
    expect(boot.phase).toBe("ready");
  });

  it("a media event landing after finish() pulls the phase back from ready to media, then settles on done", () => {
    // The reverse order: the chain resolves first (settle() sees no decode
    // in flight yet and would go straight to ready) and only afterwards
    // does the engine's first media-progress event arrive.
    boot.wire();
    boot.finish();
    expect(boot.phase).toBe("ready"); // no clips seen yet — this is the honest state so far

    fireMediaProgress({ loaded: 2, total: 19, name: "snare.wav", phase: "decode", done: false });
    expect(boot.phase).toBe("media");
    expect(boot.label).toBe("Loading audio — 2 of 19 files");

    fireMediaProgress({ loaded: 19, total: 19, name: null, phase: "peaks", done: true });
    expect(boot.phase).toBe("ready");
    expect(boot.label).toBe("Ready");
  });

  it("a project with no clips (no media events at all) still reaches ready immediately", () => {
    boot.wire();
    boot.finish();
    expect(boot.phase).toBe("ready");
    expect(boot.label).toBe("Ready");
  });

  it("documented choice: a media event long after a completed ready cycle still updates phase in the store", () => {
    // Once a cycle has genuinely completed (chainDone, then mediaInFlight
    // cleared by done:true), this store has no way to tell "a straggler
    // from THAT boot" apart from "an unrelated later project open" — and
    // doesn't try to. It keeps reacting to media-progress; resurrecting a
    // dismissed overlay is prevented one layer up, by BootOverlay's
    // one-way `inDom` flag (a component instance that has already left the
    // DOM never remounts itself, however boot.phase changes afterwards).
    boot.wire();
    boot.finish();
    fireMediaProgress({ loaded: 1, total: 1, name: null, phase: "decode", done: true });
    expect(boot.phase).toBe("ready");

    fireMediaProgress({ loaded: 0, total: 2, name: "unrelated.wav", phase: "decode", done: false });
    expect(boot.phase).toBe("media");
    expect(boot.label).toBe("Loading audio — 0 of 2 files");
  });

  it("ignores a late progress event once boot has already reached ready", () => {
    boot.wire();
    boot.finish();
    fireOpenProgress({
      step: "load",
      index: 1,
      total: 1,
      label: "should not appear",
      detail: null,
    });
    expect(boot.phase).toBe("ready");
    expect(boot.label).toBe("Ready");
  });

  it("ignores a late progress event once boot has failed", () => {
    boot.wire();
    boot.fail(new Error("nope"));
    fireMediaProgress({ loaded: 1, total: 2, name: "x", phase: "decode", done: false });
    expect(boot.phase).toBe("failed");
  });

  it("wire() is idempotent and returns a working unsubscribe", () => {
    const stopA = boot.wire();
    const stopB = boot.wire();
    expect(backendOn).toHaveBeenCalledTimes(2); // only the first wire() call subscribed
    stopB(); // no-op stub from the second call
    fireOpenProgress({ step: "load", index: 1, total: 2, label: "still wired", detail: null });
    expect(boot.label).toBe("still wired");
    stopA();
    fireOpenProgress({ step: "load", index: 2, total: 2, label: "after stop", detail: null });
    expect(boot.label).toBe("still wired"); // unsubscribed, so this must not land
  });

  it("armSafetyTimeout marks a still-pending boot as still working after the timeout", () => {
    vi.useFakeTimers();
    boot.setPhase("project");
    boot.armSafetyTimeout(1000);
    vi.advanceTimersByTime(1000);
    expect(boot.label).toBe("Still working — see the log for details");
    expect(boot.phase).toBe("project"); // the timeout only rewrites the label
    vi.useRealTimers();
  });

  it("armSafetyTimeout does nothing once boot has already settled", () => {
    vi.useFakeTimers();
    boot.setPhase("project");
    boot.armSafetyTimeout(1000);
    boot.finish();
    vi.advanceTimersByTime(1000);
    expect(boot.label).toBe("Ready");
    vi.useRealTimers();
  });

  it("clearSafetyTimeout prevents the timeout from firing at all", () => {
    vi.useFakeTimers();
    boot.setPhase("project");
    boot.armSafetyTimeout(1000);
    boot.clearSafetyTimeout();
    vi.advanceTimersByTime(5000);
    expect(boot.label).toBe("Opening the last project…");
    vi.useRealTimers();
  });
});
