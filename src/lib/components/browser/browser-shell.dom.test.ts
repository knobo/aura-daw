import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import BrowserShellHarness from "./BrowserShellHarness.svelte";

afterEach(() => cleanup());

describe("BrowserShell", () => {
  it("'/' focuses the search input from the list", async () => {
    render(BrowserShellHarness);
    const list = screen.getByRole("tree");
    list.focus();
    await fireEvent.keyDown(list, { key: "/" });
    expect(document.activeElement).toBe(screen.getByRole("textbox"));
  });

  it("Escape clears the search and returns focus to the list", async () => {
    render(BrowserShellHarness);
    const search = screen.getByRole("textbox");
    await fireEvent.input(search, { target: { value: "alpha" } });
    expect((search as HTMLInputElement).value).toBe("alpha");

    await fireEvent.keyDown(search, { key: "Escape" });

    expect((search as HTMLInputElement).value).toBe("");
    expect(document.activeElement).toBe(screen.getByRole("tree"));
  });

  it("ArrowDown/ArrowUp move aria-activedescendant across group boundaries", async () => {
    render(BrowserShellHarness);
    const list = screen.getByRole("tree");
    list.focus();

    // Starts on the first row (Group 1's header).
    expect(list.getAttribute("aria-activedescendant")).toBe("browser-row-group-g1");

    await fireEvent.keyDown(list, { key: "ArrowDown" }); // Alpha
    await fireEvent.keyDown(list, { key: "ArrowDown" }); // Bravo
    await fireEvent.keyDown(list, { key: "ArrowDown" }); // Group 2 header
    expect(list.getAttribute("aria-activedescendant")).toBe("browser-row-group-g2");

    await fireEvent.keyDown(list, { key: "ArrowUp" });
    expect(list.getAttribute("aria-activedescendant")).toBe("browser-row-item-g1-1"); // Bravo
  });

  it("ArrowUp at the top and ArrowDown at the bottom clamp instead of wrapping", async () => {
    render(BrowserShellHarness);
    const list = screen.getByRole("tree");
    list.focus();

    await fireEvent.keyDown(list, { key: "ArrowUp" });
    expect(list.getAttribute("aria-activedescendant")).toBe("browser-row-group-g1");

    for (let i = 0; i < 10; i++) await fireEvent.keyDown(list, { key: "ArrowDown" });
    expect(list.getAttribute("aria-activedescendant")).toBe("browser-row-item-g2-0"); // Charlie, the last row
  });

  it("ArrowLeft folds the active group and hides its rows; ArrowRight unfolds", async () => {
    const onToggleGroup = vi.fn();
    render(BrowserShellHarness, { props: { onToggleGroup } });
    const list = screen.getByRole("tree");
    list.focus();

    expect(screen.getByText("Alpha")).toBeTruthy();

    await fireEvent.keyDown(list, { key: "ArrowLeft" }); // fold g1 from its own header
    expect(onToggleGroup).toHaveBeenCalledWith("g1", false);
    expect(screen.queryByText("Alpha")).toBeNull();

    await fireEvent.keyDown(list, { key: "ArrowRight" });
    expect(onToggleGroup).toHaveBeenCalledWith("g1", true);
    expect(screen.getByText("Alpha")).toBeTruthy();
  });

  it("ArrowLeft on an item jumps the cursor to its group header and folds it", async () => {
    render(BrowserShellHarness);
    const list = screen.getByRole("tree");
    list.focus();

    await fireEvent.keyDown(list, { key: "ArrowDown" }); // Alpha
    expect(list.getAttribute("aria-activedescendant")).toBe("browser-row-item-g1-0");

    await fireEvent.keyDown(list, { key: "ArrowLeft" });
    expect(list.getAttribute("aria-activedescendant")).toBe("browser-row-group-g1");
    expect(screen.queryByText("Alpha")).toBeNull();
  });

  it("Enter invokes the active row's primary action", async () => {
    const onActivate = vi.fn();
    render(BrowserShellHarness, { props: { onActivate } });
    const list = screen.getByRole("tree");
    list.focus();

    await fireEvent.keyDown(list, { key: "ArrowDown" }); // Alpha
    await fireEvent.keyDown(list, { key: "Enter" });

    expect(onActivate).toHaveBeenCalledWith({ kind: "item", groupKey: "g1", itemIndex: 0 });
  });

  it("Home/End jump to the first and last row", async () => {
    render(BrowserShellHarness);
    const list = screen.getByRole("tree");
    list.focus();

    await fireEvent.keyDown(list, { key: "End" });
    expect(list.getAttribute("aria-activedescendant")).toBe("browser-row-item-g2-0");

    await fireEvent.keyDown(list, { key: "Home" });
    expect(list.getAttribute("aria-activedescendant")).toBe("browser-row-group-g1");
  });

  it("clicking a row moves the keyboard cursor to match (setActive)", async () => {
    render(BrowserShellHarness);
    const list = screen.getByRole("tree");

    await fireEvent.click(screen.getByText("Charlie"));
    expect(list.getAttribute("aria-activedescendant")).toBe("browser-row-item-g2-0");
  });
});
