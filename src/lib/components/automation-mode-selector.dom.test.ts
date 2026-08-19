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

  it("marks the current mode as selected", async () => {
    const onchange = vi.fn();
    render(AutomationModeSelector, { props: { mode: "latch", onchange } });

    const latchBtn = screen.getByTitle(/latch/i);
    expect(latchBtn.getAttribute("aria-pressed")).toBe("true");

    const readBtn = screen.getByTitle(/read/i);
    expect(readBtn.getAttribute("aria-pressed")).toBe("false");
  });
});
