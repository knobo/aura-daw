import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import Pad from "./Pad.svelte";

afterEach(cleanup);

describe("the pad", () => {
  it("fires onpress on pointerdown, not on the click that closes the press", async () => {
    // A pad is an instrument: it sounds on the strike. Firing from `click`
    // made the note wait for pointerup and for the browser to confirm both
    // halves landed on the same element — tens of milliseconds set by how
    // long a finger rests on the button.
    const onpress = vi.fn();
    render(Pad, { label: "KICK", ariaLabel: "Play kick", onpress });
    const el = screen.getByRole("button", { name: /play kick/i });
    Object.assign(el, { setPointerCapture: vi.fn(), releasePointerCapture: vi.fn() });

    await fireEvent.pointerDown(el, { button: 0, pointerId: 1 });
    expect(onpress).toHaveBeenCalledTimes(1);

    // The browser synthesises a click after pointerup; it must not double-fire.
    await fireEvent.pointerUp(el, { pointerId: 1 });
    await fireEvent.click(el);
    expect(onpress).toHaveBeenCalledTimes(1);
  });

  it("does not swallow a keyboard press after a cancelled pointer", async () => {
    // pointercancel produces no click, so the guard that suppresses the
    // pointer's own click has to be disarmed or the next Enter is eaten.
    const onpress = vi.fn();
    render(Pad, { label: "HAT", ariaLabel: "Play hat", onpress });
    const el = screen.getByRole("button", { name: /play hat/i });
    Object.assign(el, { setPointerCapture: vi.fn(), releasePointerCapture: vi.fn() });

    await fireEvent.pointerDown(el, { button: 0, pointerId: 1 });
    await fireEvent.pointerCancel(el, { pointerId: 1 });
    expect(onpress).toHaveBeenCalledTimes(1);

    await fireEvent.click(el);
    expect(onpress).toHaveBeenCalledTimes(2);
  });

  it("plays from the keyboard", async () => {
    // Enter and Space on a <button> arrive as a click with no pointer pair
    // in front of them — a pointerup-only pad is unplayable without a mouse.
    const onpress = vi.fn();
    render(Pad, { label: "SNARE", ariaLabel: "Play snare", onpress });
    await fireEvent.click(screen.getByRole("button", { name: /play snare/i }));
    expect(onpress).toHaveBeenCalledTimes(1);
  });

  it("does not fire when disabled", async () => {
    const onpress = vi.fn();
    render(Pad, { label: "—", ariaLabel: "Empty pad", disabled: true, onpress });
    const el = screen.getByRole("button", { name: /empty pad/i });
    await fireEvent.pointerDown(el, { button: 0, pointerId: 1 });
    await fireEvent.pointerUp(el, { pointerId: 1 });
    await fireEvent.click(el);
    expect(onpress).not.toHaveBeenCalled();
  });

  it("exposes latched state to a screen reader", () => {
    render(Pad, { label: "ON", ariaLabel: "Toggle", lit: true });
    expect(screen.getByRole("button", { name: /toggle/i }).getAttribute("aria-pressed")).toBe("true");
  });
});
