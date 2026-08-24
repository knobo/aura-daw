/**
 * The unified gesture: double-click a row to hear it, Shift+Enter for the
 * keyboard, and the toolbar chip that turns the whole thing on and off.
 */
import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import BrowserRow from "./BrowserRow.svelte";
import BrowserShellHarness from "./BrowserShellHarness.svelte";
import { audition } from "../../state/audition.svelte";

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("BrowserRow audition gesture", () => {
  it("double-click calls ondblclick and single click does not", async () => {
    const onclick = vi.fn();
    const ondblclick = vi.fn();
    render(BrowserRow, { props: { id: "r1", label: "Kick", onclick, ondblclick } });
    const row = screen.getByText("Kick").closest(".row") as HTMLElement;

    await fireEvent.click(row);
    expect(ondblclick).not.toHaveBeenCalled();
    expect(onclick).toHaveBeenCalledTimes(1);

    await fireEvent.dblClick(row);
    expect(ondblclick).toHaveBeenCalledTimes(1);
  });
});

describe("BrowserShell audition", () => {
  it("Shift+Enter auditions the active row without activating it", async () => {
    const onActivate = vi.fn();
    const onAudition = vi.fn();
    render(BrowserShellHarness, { props: { onActivate, onAudition } });
    const list = screen.getByRole("tree");
    list.focus();
    await fireEvent.keyDown(list, { key: "ArrowDown" });

    await fireEvent.keyDown(list, { key: "Enter", shiftKey: true });
    expect(onAudition).toHaveBeenCalledTimes(1);
    expect(onActivate).not.toHaveBeenCalled();

    await fireEvent.keyDown(list, { key: "Enter" });
    expect(onActivate).toHaveBeenCalledTimes(1);
    expect(onAudition).toHaveBeenCalledTimes(1);
  });

  it("the toolbar chip reflects and flips the audition preference", async () => {
    audition.enabled = false;
    render(BrowserShellHarness, { props: { auditionChip: true } });
    const chip = screen.getByRole("button", { name: /audition/i });
    expect(chip.getAttribute("aria-pressed")).toBe("false");

    await fireEvent.click(chip);
    expect(audition.enabled).toBe(true);
    expect(chip.getAttribute("aria-pressed")).toBe("true");

    await fireEvent.click(chip);
    expect(audition.enabled).toBe(false);
  });
});
