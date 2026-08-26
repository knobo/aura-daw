import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import Fader from "./Fader.svelte";

const FULL_TRAVEL_PX = 160;

afterEach(cleanup);

function mountFader(props: Record<string, unknown> = {}) {
  const oninput = vi.fn();
  const onstart = vi.fn();
  const onend = vi.fn();
  render(Fader, {
    value: 50,
    min: 0,
    max: 100,
    ariaLabel: "Test fader",
    oninput,
    onstart,
    onend,
    ...props,
  });
  const el = screen.getByRole("slider", { name: /test fader/i });
  Object.assign(el, {
    setPointerCapture: vi.fn(),
    releasePointerCapture: vi.fn(),
  });
  return { el, oninput, onstart, onend };
}

async function drag(el: HTMLElement, toY: number) {
  await fireEvent.pointerDown(el, { button: 0, clientY: 200, pointerId: 1 });
  await fireEvent.pointerMove(el, { clientY: toY, pointerId: 1 });
  await fireEvent.pointerUp(el, { pointerId: 1 });
}

describe("the fader's drag gesture", () => {
  it("reports itself as a vertical slider", () => {
    const { el } = mountFader();
    expect(el.getAttribute("aria-orientation")).toBe("vertical");
    expect(el.getAttribute("aria-valuenow")).toBe("50");
  });

  it("turns up when the pointer moves up", async () => {
    const { el, oninput } = mountFader();
    await drag(el, 120);
    expect(oninput).toHaveBeenLastCalledWith(50 + (80 / FULL_TRAVEL_PX) * 100);
  });

  it("brackets the whole drag in exactly one gesture", async () => {
    const { el, onstart, onend } = mountFader();
    await fireEvent.pointerDown(el, { button: 0, clientY: 200, pointerId: 1 });
    await fireEvent.pointerMove(el, { clientY: 180, pointerId: 1 });
    expect(onstart).toHaveBeenCalledTimes(1);
    expect(onend).not.toHaveBeenCalled();
    await fireEvent.pointerUp(el, { pointerId: 1 });
    expect(onend).toHaveBeenCalledTimes(1);
  });
});
