/**
 * Rehearse-hold refcounting. The case that matters is the overlap: with a
 * private flag per control, releasing one while the other is still down
 * ends the hold and the take silently starts recording audio again.
 */
import { describe, it, expect, beforeEach, vi } from "vitest";

const setRehearseHold = vi.fn((_on: boolean) => Promise.resolve());
vi.mock("../tauri", () => ({ backend: { setRehearseHold: (on: boolean) => setRehearseHold(on) } }));

const { setRehearseSource, releaseRehearse, rehearseSourcesHeld } = await import("./rehearse.svelte");

describe("rehearse hold", () => {
  beforeEach(() => {
    releaseRehearse();
    setRehearseHold.mockClear();
  });

  it("tells the engine on the first press and the last release", () => {
    expect(setRehearseSource("key", true)).toBe(true);
    expect(setRehearseHold).toHaveBeenLastCalledWith(true);
    expect(setRehearseSource("key", false)).toBe(true);
    expect(setRehearseHold).toHaveBeenLastCalledWith(false);
    expect(setRehearseHold).toHaveBeenCalledTimes(2);
  });

  it("keeps the hold while the OTHER source is still down", () => {
    setRehearseSource("button", true);
    setRehearseSource("key", true);
    setRehearseHold.mockClear();

    // Releasing the button must NOT end the hold — the key is still down,
    // and the take would start writing real audio under the singer.
    expect(setRehearseSource("button", false)).toBe(false);
    expect(setRehearseHold).not.toHaveBeenCalled();
    expect(rehearseSourcesHeld()).toBe(1);

    expect(setRehearseSource("key", false)).toBe(true);
    expect(setRehearseHold).toHaveBeenCalledExactlyOnceWith(false);
  });

  it("does not re-send on a repeated press of the same source", () => {
    setRehearseSource("key", true);
    setRehearseHold.mockClear();
    expect(setRehearseSource("key", true)).toBe(false);
    expect(setRehearseHold).not.toHaveBeenCalled();
  });

  it("ignores a release of a source that was never pressed", () => {
    expect(setRehearseSource("button", false)).toBe(false);
    expect(setRehearseHold).not.toHaveBeenCalled();
  });

  it("releaseRehearse drops every source at once, and only when holding", () => {
    setRehearseSource("key", true);
    setRehearseSource("button", true);
    setRehearseHold.mockClear();

    expect(releaseRehearse()).toBe(true);
    expect(rehearseSourcesHeld()).toBe(0);
    expect(setRehearseHold).toHaveBeenCalledExactlyOnceWith(false);

    setRehearseHold.mockClear();
    expect(releaseRehearse()).toBe(false);
    expect(setRehearseHold).not.toHaveBeenCalled();
  });

  it("can be re-armed after a blur release", () => {
    setRehearseSource("key", true);
    releaseRehearse();
    setRehearseHold.mockClear();
    expect(setRehearseSource("key", true)).toBe(true);
    expect(setRehearseHold).toHaveBeenCalledExactlyOnceWith(true);
  });
});
