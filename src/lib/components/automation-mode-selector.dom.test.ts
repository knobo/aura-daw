import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";

const { default: AutomationModeSelector } = await import("./AutomationModeSelector.svelte");

afterEach(() => {
  cleanup();
});

describe("AutomationModeSelector", () => {
  it("renders all five modes and calls onchange with the clicked mode", async () => {
    const onchange = vi.fn();
    render(AutomationModeSelector, { props: { mode: "read", onchange } });

    const writeBtn = screen.getByTitle(/write/i);
    await fireEvent.click(writeBtn);

    expect(onchange).toHaveBeenCalledWith("write");
  });

  /** The chips SHOW a letter and are CALLED by their mode name. Both halves
   * matter: the letter is why five modes fit a lane header at all, and the
   * accessible name is the only thing standing between "T" and a control
   * nobody can identify. A tidy-up that drops either one regresses this. */
  it("shows one letter per mode but is still named by the mode", () => {
    render(AutomationModeSelector, { props: { mode: "read", onchange: vi.fn() } });

    for (const [letter, name] of [
      ["O", "Off"],
      ["R", "Read"],
      ["W", "Write"],
      ["T", "Touch"],
      ["L", "Latch"],
    ]) {
      const btn = screen.getByRole("button", { name });
      expect(btn.textContent?.trim()).toBe(letter);
    }
  });

  it("marks the current mode as selected", async () => {
    const onchange = vi.fn();
    render(AutomationModeSelector, { props: { mode: "latch", onchange } });

    const latchBtn = screen.getByTitle(/latch/i);
    expect(latchBtn.getAttribute("aria-pressed")).toBe("true");

    const readBtn = screen.getByTitle(/read/i);
    expect(readBtn.getAttribute("aria-pressed")).toBe("false");
  });
});
