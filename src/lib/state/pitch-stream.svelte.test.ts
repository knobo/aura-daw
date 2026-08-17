/**
 * The frame bus's SUBSCRIPTION lifetime, mocked at the backend seam.
 *
 * Separate from `pitch.svelte.test.ts` because these need a fake backend
 * and those deliberately need none. Every case here is a leak: the engine
 * appends a sink per `pitch_subscribe` and only drops one when the frontend
 * says so, so a subscription this module forgets is one the app carries,
 * serialized and shipped at 60 Hz, until it exits.
 */
import { describe, it, expect, beforeEach, vi } from "vitest";

const unsubscribeChannel = vi.fn();
const unsubscribeState = vi.fn();
let resolveSubscribe: ((off: () => void) => void) | null = null;
const subscribePitch = vi.fn(
  (_cb: unknown) =>
    new Promise<() => void>((resolve) => {
      resolveSubscribe = resolve;
    }),
);
const on = vi.fn((_name: string, _cb: unknown) => unsubscribeState);

vi.mock("../tauri", () => ({
  backend: {
    subscribePitch: (cb: unknown) => subscribePitch(cb),
    on: (name: string, cb: unknown) => on(name, cb),
  },
}));

const { startPitchStream, stopPitchStream, pitchMode } = await import("./pitch.svelte");

/** Resolve the pending `subscribePitch` and let its `.then` run. */
async function settleSubscribe() {
  resolveSubscribe?.(unsubscribeChannel);
  await Promise.resolve();
  await Promise.resolve();
}

describe("pitch stream lifetime", () => {
  beforeEach(() => {
    vi.clearAllMocks();
    resolveSubscribe = null;
  });

  it("tells the backend to unsubscribe, not just the local handler", () => {
    // The unsubscribe the backend hands back is the one that reaches Rust —
    // dropping it on the floor is the leak this whole file guards.
    const start = startPitchStream();
    return settleSubscribe()
      .then(() => start)
      .then(() => {
        stopPitchStream();
        expect(unsubscribeChannel).toHaveBeenCalledTimes(1);
        expect(unsubscribeState).toHaveBeenCalledTimes(1);
      });
  });

  it("does not subscribe twice while one subscription is live", async () => {
    const first = startPitchStream();
    await settleSubscribe();
    await first;
    await startPitchStream();
    expect(subscribePitch).toHaveBeenCalledTimes(1);
    stopPitchStream();
  });

  it("does not subscribe twice while the first subscribe is still in flight", async () => {
    const first = startPitchStream();
    await startPitchStream(); // panel remounted before the await resolved
    expect(subscribePitch).toHaveBeenCalledTimes(1);
    await settleSubscribe();
    await first;
    stopPitchStream();
  });

  it("unsubscribes a subscription that resolves AFTER the stop", async () => {
    // Double-click the tab, or an HMR remount: teardown runs while
    // `subscribePitch` is still pending. Without the generation check the
    // late resolution installs a handler nobody owns.
    const start = startPitchStream();
    stopPitchStream();
    await settleSubscribe();
    await start;
    expect(unsubscribeChannel).toHaveBeenCalledTimes(1);
    expect(on).not.toHaveBeenCalled();
  });

  it("resets the mode flags on stop so a closed panel reports nothing live", async () => {
    const start = startPitchStream();
    await settleSubscribe();
    await start;
    pitchMode.listening = true;
    stopPitchStream();
    expect(pitchMode.listening).toBe(false);
  });

  it("can start again after a stop", async () => {
    const first = startPitchStream();
    await settleSubscribe();
    await first;
    stopPitchStream();
    const second = startPitchStream();
    await settleSubscribe();
    await second;
    expect(subscribePitch).toHaveBeenCalledTimes(2);
    stopPitchStream();
    expect(unsubscribeChannel).toHaveBeenCalledTimes(2);
  });
});
