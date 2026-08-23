/**
 * The audition store: one gesture, four row kinds, one gate.
 *
 * Every assertion here is about the two rules the store exists to hold:
 * the preference gates it, and no path it takes is ever a project edit.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";

const invokes = {
  samplerPreviewNote: vi.fn(async (_id: string, _key: number, _vel: number) => {}),
  pluginPreviewNote: vi.fn(async (_id: string, _key: number, _vel: number) => {}),
  libraryAudition: vi.fn(async (_p: string, _s?: number | null) => {}),
  libraryAuditionStop: vi.fn(async () => {}),
};

vi.mock("../tauri", () => ({ backend: invokes }));

const { audition } = await import("./audition.svelte");
const { prefs } = await import("../prefs/prefs.svelte");

beforeEach(async () => {
  // Fully settle whatever the previous test left in flight (an open decay
  // timer, a sample stream still marked as sounding) before clearing mock
  // history and resetting public state. Without this, leftover private
  // state changes how many `await`s a later test's `play()` needs before
  // it reaches the backend call — the race-safety tests below depend on a
  // clean, predictable starting point to control that interleaving.
  await audition.stop();
  for (const fn of Object.values(invokes)) fn.mockClear();
  audition.enabled = true;
  audition.sounding = null;
  audition.lastSilentReason = null;
});

describe("audition gate", () => {
  it("plays nothing at all while the preference is off", async () => {
    audition.enabled = false;
    await audition.play({ kind: "sample", path: "/lib/kick.wav" });
    expect(invokes.libraryAudition).not.toHaveBeenCalled();
    expect(audition.sounding).toBeNull();
  });

  it("the enabled setter writes the preference, so the chip and the dialog agree", () => {
    audition.enabled = false;
    expect(prefs.values.browserAudition).toBe(false);
    audition.enabled = true;
    expect(prefs.values.browserAudition).toBe(true);
  });
});

describe("audition play", () => {
  it("a sample goes through library_audition and marks its path sounding", async () => {
    await audition.play({ kind: "sample", path: "/lib/kick.wav" });
    expect(invokes.libraryAudition).toHaveBeenCalledWith("/lib/kick.wav");
    expect(audition.sounding).toBe("/lib/kick.wav");
  });

  it("an instrument goes through sampler_preview_note at the given key", async () => {
    await audition.play({ kind: "instrument", instrumentId: "i1", key: 60 });
    expect(invokes.samplerPreviewNote).toHaveBeenCalledWith("i1", 60, 100);
    expect(audition.sounding).toBe("i1");
  });

  it("a plugin instance goes through plugin_preview_note", async () => {
    await audition.play({ kind: "pluginInstance", instanceId: "p1", key: 60 });
    expect(invokes.pluginPreviewNote).toHaveBeenCalledWith("p1", 60, 100);
    expect(audition.sounding).toBe("p1");
  });

  it("a silent target records its reason and calls no backend at all", async () => {
    await audition.play({ kind: "silent", reason: "no live instance of this plugin to audition" });
    expect(audition.lastSilentReason).toMatch(/no live instance/);
    expect(invokes.libraryAudition).not.toHaveBeenCalled();
    expect(invokes.samplerPreviewNote).not.toHaveBeenCalled();
    expect(invokes.pluginPreviewNote).not.toHaveBeenCalled();
    expect(audition.sounding).toBeNull();
  });

  it("a new audition stops the previous sample before starting", async () => {
    await audition.play({ kind: "sample", path: "/lib/a.wav" });
    invokes.libraryAuditionStop.mockClear();
    await audition.play({ kind: "sample", path: "/lib/b.wav" });
    expect(invokes.libraryAuditionStop).toHaveBeenCalled();
    expect(audition.sounding).toBe("/lib/b.wav");
  });

  it("a backend failure clears sounding and surfaces the reason, never throws", async () => {
    invokes.libraryAudition.mockRejectedValueOnce(new Error("decode failed"));
    await expect(audition.play({ kind: "sample", path: "/lib/bad.wav" })).resolves.toBeUndefined();
    expect(audition.sounding).toBeNull();
    expect(audition.lastSilentReason).toMatch(/decode failed/);
  });
});

describe("audition race safety", () => {
  // Regression coverage for a real ordering race: two `play()` calls fired
  // close together, where the *older* one's IPC round-trip resolves after
  // the *newer* one's. A sequential await-then-await test (above) cannot
  // observe this — these deliberately hold each call's `libraryAudition`
  // promise open under manual control to force the interleaving.

  it("an overlapping play cuts off an in-flight sample and the newer one wins, even if the older one resolves later", async () => {
    let resolveA!: () => void;
    const pendingA = new Promise<void>((resolve) => {
      resolveA = resolve;
    });
    invokes.libraryAudition.mockImplementationOnce(() => pendingA);

    const playA = audition.play({ kind: "sample", path: "/lib/a.wav" });
    // Flush microtasks so playA reaches (and suspends on) the backend call
    // before playB is fired — this is the "A's IPC round-trip is already
    // in flight" half of the race.
    await Promise.resolve();
    expect(invokes.libraryAudition).toHaveBeenCalledWith("/lib/a.wav");

    const playB = audition.play({ kind: "sample", path: "/lib/b.wav" });
    await playB;

    // B fully resolved while A was still pending: B must have cut A off
    // at the backend and taken the highlight.
    expect(invokes.libraryAuditionStop).toHaveBeenCalled();
    expect(audition.sounding).toBe("/lib/b.wav");

    // A's stale round-trip finally completes. It must not resurrect A —
    // neither the highlight nor a duplicate "now sounding" state.
    resolveA();
    await playA;
    expect(audition.sounding).toBe("/lib/b.wav");
  });

  it("stop() called while a sample play() is in flight cuts it off, and the play() does not start it after", async () => {
    let resolveA!: () => void;
    const pendingA = new Promise<void>((resolve) => {
      resolveA = resolve;
    });
    invokes.libraryAudition.mockImplementationOnce(() => pendingA);

    const playA = audition.play({ kind: "sample", path: "/lib/a.wav" });
    await Promise.resolve();
    expect(invokes.libraryAudition).toHaveBeenCalledWith("/lib/a.wav");

    await audition.stop();
    expect(invokes.libraryAuditionStop).toHaveBeenCalled();
    expect(audition.sounding).toBeNull();

    // The in-flight play() finally completes after stop() already ran —
    // it must not light the row back up.
    resolveA();
    await playA;
    expect(audition.sounding).toBeNull();
  });
});

describe("audition decay", () => {
  it("clears the sounding highlight on a timer, and a newer audition wins the timer", async () => {
    vi.useFakeTimers();
    try {
      await audition.play({ kind: "instrument", instrumentId: "i1", key: 60 });
      expect(audition.sounding).toBe("i1");
      vi.advanceTimersByTime(1200);
      expect(audition.sounding).toBeNull();
    } finally {
      vi.useRealTimers();
    }
  });
});

describe("audition stop", () => {
  it("releases the sample stream and clears the highlight", async () => {
    await audition.play({ kind: "sample", path: "/lib/a.wav" });
    await audition.stop();
    expect(invokes.libraryAuditionStop).toHaveBeenCalled();
    expect(audition.sounding).toBeNull();
  });
});
