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

beforeEach(() => {
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
