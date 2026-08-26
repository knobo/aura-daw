import { afterEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import AddMenu from "./AddMenu.svelte";
import { surface } from "../../state/surface.svelte";
import { emptyLayout } from "../../utils/control-surface";

afterEach(() => {
  cleanup();
  surface.layout = emptyLayout();
  surface.addOpen = false;
});

describe("the add menu", () => {
  it("lists the fill recipes and the LPD8 template", async () => {
    render(AddMenu);
    await fireEvent.click(screen.getByRole("button", { name: /add a control/i }));
    expect(screen.getByText("Add all tracks")).toBeTruthy();
    expect(screen.getByText("Add all clips")).toBeTruthy();
    expect(screen.getByText("Add all automations")).toBeTruthy();
    expect(screen.getByText("AKAI LPD8")).toBeTruthy();
  });

  it("stamps a blank page from the template item", async () => {
    render(AddMenu);
    await fireEvent.click(screen.getByRole("button", { name: /add a control/i }));
    await fireEvent.click(screen.getByRole("menuitem", { name: /blank page/i }));
    expect(surface.page.templateId).toBe("blank");
    expect(surface.page.widgets).toEqual([]);
    expect(surface.addOpen).toBe(false);
  });
});
