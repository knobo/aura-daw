import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import Lamp from "./Lamp.svelte";

/**
 * The channel-strip key. jsdom resolves no layout, so the 92px-in-a-76px-box
 * overflow that prompted the rewrite is not testable here — that was measured
 * in a real browser and the fix (a `1fr` grid with `min-width: 0`) lives in
 * ChannelStrip's stylesheet.
 *
 * What IS testable is the half of the fix that is not CSS: a compact key
 * shows one letter instead of a four-letter word, and it must not lose the
 * word while doing it. A key that renders "M" with no accessible name saying
 * "mute" is not a smaller button, it is an unlabelled one.
 */
afterEach(cleanup);

describe("the compact key", () => {
  it("shows one letter per role, and AURA's letters — not R for arm", () => {
    for (const [role, glyph] of [
      ["mute", "M"],
      ["solo", "S"],
      ["arm", "A"],
    ] as const) {
      render(Lamp, { on: false, label: role.toUpperCase(), role, compact: true, ariaLabel: `Track ${role}` });
      expect(screen.getByRole("button", { name: `Track ${role}` }).textContent?.trim()).toBe(glyph);
      cleanup();
    }
  });

  // R is automation Read in AutomationModeSelector, in the same track header
  // row, and the track header itself says A. One letter, one meaning.
  it("never prints R, which this app has already spent on automation Read", () => {
    render(Lamp, { on: false, label: "ARM", role: "arm", compact: true, ariaLabel: "Arm" });
    expect(screen.getByRole("button", { name: "Arm" }).textContent?.trim()).not.toBe("R");
  });

  it("keeps the full word on the accessible name and the tooltip", () => {
    render(Lamp, { on: false, label: "MUTE", role: "mute", compact: true, ariaLabel: "Vox Lead MUTE" });
    const el = screen.getByRole("button", { name: "Vox Lead MUTE" });
    expect(el.getAttribute("title")).toBe("MUTE");
  });

  it("spells the label out when it is not compact — a free deck widget has room", () => {
    render(Lamp, { on: false, label: "MUTE", role: "mute", ariaLabel: "Mute" });
    expect(screen.getByRole("button", { name: "Mute" }).textContent?.trim()).toBe("MUTE");
  });
});

describe("the key's state and gesture", () => {
  it("reports its on/off state to a screen reader", () => {
    render(Lamp, { on: true, label: "SOLO", role: "solo", compact: true, ariaLabel: "Solo" });
    expect(screen.getByRole("button", { name: "Solo" }).getAttribute("aria-pressed")).toBe("true");
  });

  it("toggles on click in either variant", async () => {
    for (const variant of ["key", "bar"] as const) {
      const onclick = vi.fn();
      render(Lamp, { on: false, label: "MUTE", role: "mute", variant, compact: true, ariaLabel: "Mute", onclick });
      await fireEvent.click(screen.getByRole("button", { name: "Mute" }));
      expect(onclick, variant).toHaveBeenCalledTimes(1);
      cleanup();
    }
  });
});
