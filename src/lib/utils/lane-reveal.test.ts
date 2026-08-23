/**
 * `revealParamLane` — the one "jump to this target's lane" helper shared by
 * the automation matrix, pinned chips and the header picker (plan 6.1-6.3):
 * unfold the lane, show/mint the overlay, scroll the header into view.
 * Stores are stubbed; this file is a plain `*.test.ts` (no DOM needed for
 * the store-interaction assertions — jsdom quirks are only exercised via a
 * bare `document.querySelector` stub for the scroll step).
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Binding, TargetRef } from "../types/ipc";

const isTrackCollapsed = vi.fn();
const toggleTrack = vi.fn();
const pickTarget = vi.fn();
const isBindingVisible = vi.fn();
const show = vi.fn();
const toastError = vi.fn();

vi.mock("../state/lanes.svelte", () => ({
  lanes: {
    isTrackCollapsed: (...a: unknown[]) => isTrackCollapsed(...a),
    toggleTrack: (...a: unknown[]) => toggleTrack(...a),
  },
}));

vi.mock("../state/modulation.svelte", () => ({
  modulation: {
    pickTarget: (...a: unknown[]) => pickTarget(...a),
    isBindingVisible: (...a: unknown[]) => isBindingVisible(...a),
    show: (...a: unknown[]) => show(...a),
  },
}));

vi.mock("../state/toasts.svelte", () => ({
  toasts: {
    error: (...a: unknown[]) => toastError(...a),
  },
}));

const { revealParamLane } = await import("./lane-reveal");

const TARGET: TargetRef = { kind: "pluginParam", instanceId: "inst-1", paramId: 3 };
const BINDING: Binding = {
  id: "b1",
  source: { kind: "curve", curveId: "c1" },
  target: TARGET,
  mode: "absolute",
  depth: 1,
};

beforeEach(() => {
  vi.clearAllMocks();
  isTrackCollapsed.mockReturnValue(false);
  isBindingVisible.mockReturnValue(true);
  pickTarget.mockResolvedValue(BINDING);
});

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("revealParamLane", () => {
  it("does nothing but surface a toast when trackId is empty (an orphaned instance)", async () => {
    await revealParamLane("", TARGET);

    // No fold check, no mint, no show — an empty trackId must not reach
    // ANY of the store calls this helper otherwise makes, mirroring the
    // real bug: `pickTarget` doesn't validate the track and would happily
    // commit a curve+binding filed under a track key nothing can find.
    expect(isTrackCollapsed).not.toHaveBeenCalled();
    expect(toggleTrack).not.toHaveBeenCalled();
    expect(pickTarget).not.toHaveBeenCalled();
    expect(show).not.toHaveBeenCalled();
    expect(toastError).toHaveBeenCalledWith(
      "NOT ON A TRACK",
      "This plugin isn't on a track, so there's no lane to put automation on.",
    );
  });

  it("unfolds a folded lane before revealing it", async () => {
    isTrackCollapsed.mockReturnValue(true);
    await revealParamLane("t1", TARGET);
    expect(toggleTrack).toHaveBeenCalledWith("t1");
  });

  it("does not touch fold state when the lane is already unfolded", async () => {
    isTrackCollapsed.mockReturnValue(false);
    await revealParamLane("t1", TARGET);
    expect(toggleTrack).not.toHaveBeenCalled();
  });

  it("calls pickTarget with the trackId, target and seed value", async () => {
    await revealParamLane("t1", TARGET, 0.75);
    expect(pickTarget).toHaveBeenCalledWith("t1", TARGET, 0.75);
  });

  it("re-shows a binding that pickTarget's toggle left hidden", async () => {
    isBindingVisible.mockReturnValue(false);
    await revealParamLane("t1", TARGET);
    expect(show).toHaveBeenCalledWith("t1", "b1");
  });

  it("does not call show when the binding is already visible", async () => {
    isBindingVisible.mockReturnValue(true);
    await revealParamLane("t1", TARGET);
    expect(show).not.toHaveBeenCalled();
  });

  it("is harmless when the track header element is missing from the DOM", async () => {
    const querySelector = vi.fn(() => null);
    vi.stubGlobal("CSS", { escape: (s: string) => s });
    vi.stubGlobal("document", { querySelector });
    await expect(revealParamLane("t1", TARGET)).resolves.toBeUndefined();
    // Prove the null-element branch (querySelector -> null, then
    // el?.scrollIntoView?.() short-circuiting) was actually exercised,
    // not skipped by CSS.escape throwing before querySelector ran.
    expect(querySelector).toHaveBeenCalledWith('[data-track-id="t1"]');
  });

  it("is harmless when the found element has no scrollIntoView (jsdom)", async () => {
    const el = {}; // no scrollIntoView property at all
    vi.stubGlobal("CSS", { escape: (s: string) => s });
    vi.stubGlobal("document", { querySelector: () => el });
    await expect(revealParamLane("t1", TARGET)).resolves.toBeUndefined();
  });

  it("scrolls the matching track header into view when present", async () => {
    const scrollIntoView = vi.fn();
    const querySelector = vi.fn(() => ({ scrollIntoView }));
    vi.stubGlobal("CSS", { escape: (s: string) => s });
    vi.stubGlobal("document", { querySelector });
    await revealParamLane("t1", TARGET);
    expect(querySelector).toHaveBeenCalledWith('[data-track-id="t1"]');
    expect(scrollIntoView).toHaveBeenCalledWith({ block: "nearest" });
  });

  it("never throws when pickTarget rejects", async () => {
    pickTarget.mockRejectedValue(new Error("mint failed"));
    await expect(revealParamLane("t1", TARGET)).resolves.toBeUndefined();
  });
});
