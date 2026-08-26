import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import Pad from "./Pad.svelte";

afterEach(cleanup);

describe("the pad", () => {
  it("fires onpress on pointer up after a press", async () => {
    const onpress = vi.fn();
    render(Pad, { label: "KICK", ariaLabel: "Play kick", onpress });
    const el = screen.getByRole("button", { name: /play kick/i });
    Object.assign(el, {
      setPointerCapture: vi.fn(),
      releasePointerCapture: vi.fn(),
    });
    await fireEvent.pointerDown(el, { button: 0, pointerId: 1 });
    expect(onpress).not.toHaveBeenCalled();
    await fireEvent.pointerUp(el, { pointerId: 1 });
    expect(onpress).toHaveBeenCalledTimes(1);
  });

  it("does not fire when disabled", async () => {
    const onpress = vi.fn();
    render(Pad, { label: "—", ariaLabel: "Empty pad", disabled: true, onpress });
    const el = screen.getByRole("button", { name: /empty pad/i });
    await fireEvent.pointerDown(el, { button: 0, pointerId: 1 });
    await fireEvent.pointerUp(el, { pointerId: 1 });
    expect(onpress).not.toHaveBeenCalled();
  });

  it("exposes latched state to a screen reader", () => {
    render(Pad, { label: "ON", ariaLabel: "Toggle", lit: true });
    expect(screen.getByRole("button", { name: /toggle/i }).getAttribute("aria-pressed")).toBe("true");
  });
});
