import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import Knob from "./Knob.svelte";

/**
 * The knob's gesture, mounted for real. Everything here is about the drag
 * arithmetic and the gesture brackets — the parts a caller can get wrong in
 * a way no type checks. The material styling is not under test: it is all
 * `var()` reads that jsdom does not resolve anyway.
 *
 * jsdom has no Pointer Events implementation, so `setPointerCapture` /
 * `releasePointerCapture` are absent on the element. The component calls
 * them, which is why every test here installs stubs on the element first;
 * `releasePointerCapture?.()` is optional-called in the component for the
 * same reason, but the capture call is not — a real browser needs it, and
 * dropping it would lose the pointer the moment it leaves the 38px cap.
 */

/** The component's own FULL_TRAVEL_PX. A 200px drag covers the whole range. */
const FULL_TRAVEL_PX = 200;

afterEach(cleanup);

/** Renders a unipolar 0..100 knob and returns the slider plus the spies. */
function mountKnob(props: Record<string, unknown> = {}) {
  const oninput = vi.fn();
  const onstart = vi.fn();
  const onend = vi.fn();
  render(Knob, {
    value: 50,
    min: 0,
    max: 100,
    ariaLabel: "Test knob",
    oninput,
    onstart,
    onend,
    ...props,
  });
  const el = screen.getByRole("slider", { name: /test knob/i });
  // See the jsdom note above.
  Object.assign(el, {
    setPointerCapture: vi.fn(),
    releasePointerCapture: vi.fn(),
  });
  return { el, oninput, onstart, onend };
}

/** One complete drag: press at y=200, move to `toY`, release. */
async function drag(el: HTMLElement, toY: number, opts: { shiftKey?: boolean } = {}) {
  await fireEvent.pointerDown(el, { button: 0, clientY: 200, pointerId: 1 });
  await fireEvent.pointerMove(el, { clientY: toY, pointerId: 1, ...opts });
  await fireEvent.pointerUp(el, { pointerId: 1 });
}

describe("the knob's drag gesture", () => {
  it("reports itself as a slider carrying its range and value", () => {
    const { el } = mountKnob();
    expect(el.getAttribute("aria-valuemin")).toBe("0");
    expect(el.getAttribute("aria-valuemax")).toBe("100");
    expect(el.getAttribute("aria-valuenow")).toBe("50");
  });

  it("turns up when the pointer moves up", async () => {
    const { el, oninput } = mountKnob();
    // 100px up over a 200px full travel is half the 0..100 range.
    await drag(el, 100);
    expect(oninput).toHaveBeenLastCalledWith(50 + (100 / FULL_TRAVEL_PX) * 100);
  });

  it("turns down when the pointer moves down", async () => {
    const { el, oninput } = mountKnob();
    await drag(el, 300);
    expect(oninput).toHaveBeenLastCalledWith(50 - (100 / FULL_TRAVEL_PX) * 100);
  });

  it("brackets the whole drag in exactly one gesture", async () => {
    const { el, onstart, onend } = mountKnob();
    await fireEvent.pointerDown(el, { button: 0, clientY: 200, pointerId: 1 });
    await fireEvent.pointerMove(el, { clientY: 180, pointerId: 1 });
    await fireEvent.pointerMove(el, { clientY: 160, pointerId: 1 });
    expect(onstart).toHaveBeenCalledTimes(1);
    expect(onend).not.toHaveBeenCalled();
    await fireEvent.pointerUp(el, { pointerId: 1 });
    expect(onend).toHaveBeenCalledTimes(1);
  });

  it("scales the move by a fifth while Shift is held", async () => {
    const { el, oninput } = mountKnob();
    await drag(el, 100, { shiftKey: true });
    expect(oninput).toHaveBeenLastCalledWith(50 + (100 / FULL_TRAVEL_PX) * 100 * 0.2);
  });

  /**
   * The drag is measured from where it began, not accumulated per move. A
   * caller that rounds or quantises the value it writes back would otherwise
   * feed a different `value` in on every move and the knob would crawl away
   * from the pointer — which is what makes this worth a test: the component
   * looks correct either way until a stepped caller uses it.
   */
  it("measures from the drag origin, so a rounding caller cannot drift", async () => {
    const { el, oninput } = mountKnob();
    await fireEvent.pointerDown(el, { button: 0, clientY: 200, pointerId: 1 });
    await fireEvent.pointerMove(el, { clientY: 190, pointerId: 1 });
    await fireEvent.pointerMove(el, { clientY: 180, pointerId: 1 });
    await fireEvent.pointerMove(el, { clientY: 170, pointerId: 1 });
    // 30px from the origin, not three separate 10px steps off a moving base.
    expect(oninput).toHaveBeenLastCalledWith(50 + (30 / FULL_TRAVEL_PX) * 100);
  });

  it("clamps to the ends of the range", async () => {
    const { el, oninput } = mountKnob();
    await drag(el, -2000);
    expect(oninput).toHaveBeenLastCalledWith(100);
    await drag(el, 2000);
    expect(oninput).toHaveBeenLastCalledWith(0);
  });

  it("ignores a move that never started with a press", async () => {
    const { el, oninput } = mountKnob();
    await fireEvent.pointerMove(el, { clientY: 20, pointerId: 1 });
    expect(oninput).not.toHaveBeenCalled();
  });

  it("ignores a non-primary button", async () => {
    const { el, oninput, onstart } = mountKnob();
    await fireEvent.pointerDown(el, { button: 2, clientY: 200, pointerId: 1 });
    await fireEvent.pointerMove(el, { clientY: 100, pointerId: 1 });
    expect(onstart).not.toHaveBeenCalled();
    expect(oninput).not.toHaveBeenCalled();
  });
});

describe("the knob's reset", () => {
  it("returns a bipolar knob to centre on a double-click", async () => {
    const { el, oninput, onstart, onend } = mountKnob({
      value: 0.7,
      min: -1,
      max: 1,
      bipolar: true,
    });
    await fireEvent.dblClick(el);
    expect(oninput).toHaveBeenCalledWith(0);
    // A reset is an edit like any other and must be one undo entry.
    expect(onstart).toHaveBeenCalledTimes(1);
    expect(onend).toHaveBeenCalledTimes(1);
  });

  it("returns a unipolar knob to its minimum unless told otherwise", async () => {
    const { el, oninput } = mountKnob({ value: 50, min: 0, max: 100 });
    await fireEvent.dblClick(el);
    expect(oninput).toHaveBeenCalledWith(0);
  });

  it("honours an explicit resetTo", async () => {
    const { el, oninput } = mountKnob({ value: 50, min: -60, max: 12, resetTo: 0 });
    await fireEvent.dblClick(el);
    expect(oninput).toHaveBeenCalledWith(0);
  });
});

describe("the knob's keyboard control", () => {
  it("nudges by a hundredth of the range on an arrow", async () => {
    const { el, oninput } = mountKnob();
    await fireEvent.keyDown(el, { key: "ArrowUp" });
    expect(oninput).toHaveBeenLastCalledWith(51);
    await fireEvent.keyDown(el, { key: "ArrowDown" });
    expect(oninput).toHaveBeenLastCalledWith(49);
  });

  it("nudges by a tenth of the range on a page key", async () => {
    const { el, oninput } = mountKnob();
    await fireEvent.keyDown(el, { key: "PageUp" });
    expect(oninput).toHaveBeenLastCalledWith(60);
  });

  it("sends Home to the reset value", async () => {
    const { el, oninput } = mountKnob({ value: 0.7, min: -1, max: 1, bipolar: true });
    await fireEvent.keyDown(el, { key: "Home" });
    expect(oninput).toHaveBeenLastCalledWith(0);
  });

  it("leaves keys it does not own alone, so shortcuts still reach the app", async () => {
    const { el, oninput } = mountKnob();
    await fireEvent.keyDown(el, { key: " " });
    await fireEvent.keyDown(el, { key: "r" });
    expect(oninput).not.toHaveBeenCalled();
  });

  it("brackets a keyboard nudge in its own gesture", async () => {
    const { el, onstart, onend } = mountKnob();
    await fireEvent.keyDown(el, { key: "ArrowUp" });
    expect(onstart).toHaveBeenCalledTimes(1);
    expect(onend).toHaveBeenCalledTimes(1);
  });
});

describe("the knob's readout", () => {
  it("shows the formatted value and puts it on the slider for a screen reader", () => {
    const fmt = (v: number) => (v < 0 ? `${-v}L` : v > 0 ? `${v}R` : "C");
    render(Knob, {
      value: 0,
      min: -1,
      max: 1,
      bipolar: true,
      format: fmt,
      ariaLabel: "Pan",
    });
    const el = screen.getByRole("slider", { name: /pan/i });
    expect(el.getAttribute("aria-valuetext")).toBe("C");
    expect(screen.getByText("C")).toBeTruthy();
  });
});
