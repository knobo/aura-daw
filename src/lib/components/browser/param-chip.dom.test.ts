import { afterEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen } from "@testing-library/svelte";
import ParamChip from "./ParamChip.svelte";

afterEach(() => cleanup());

describe("ParamChip", () => {
  it("renders as a plain span when there is no onclick", () => {
    render(ParamChip, { props: { label: "Cutoff", value: 0.62, format: (v) => `${Math.round(v * 100)}%` } });
    expect(screen.queryByRole("button")).toBeNull();
    expect(screen.getByLabelText("Cutoff, 62%")).toBeTruthy();
  });

  it("renders as a real button when onclick is given, and calls it", async () => {
    const onclick = vi.fn();
    render(ParamChip, {
      props: { label: "Cutoff", value: 0.62, format: (v) => `${Math.round(v * 100)}%`, onclick },
    });
    const btn = screen.getByRole("button", { name: "Cutoff, 62%" });
    await fireEvent.click(btn);
    expect(onclick).toHaveBeenCalledTimes(1);
  });

  it("names the state in the accessible label — colour is never the only carrier", () => {
    render(ParamChip, {
      props: {
        label: "Cutoff",
        value: 0.62,
        format: (v) => `${Math.round(v * 100)}%`,
        state: "automated",
      },
    });
    expect(screen.getByLabelText("Cutoff, 62%, automated")).toBeTruthy();
  });

  it("spells out MIDI-mapped and recording states the same way", () => {
    const { unmount } = render(ParamChip, {
      props: { label: "Drive", value: 3, format: (v) => String(v), state: "mapped" },
    });
    expect(screen.getByLabelText("Drive, 3, MIDI-mapped")).toBeTruthy();
    unmount();

    render(ParamChip, {
      props: { label: "Gain", value: -2, format: (v) => `${v} dB`, state: "recording" },
    });
    expect(screen.getByLabelText("Gain, -2 dB, recording")).toBeTruthy();
  });

  it("defaults to String(value) when no format is supplied", () => {
    render(ParamChip, { props: { label: "Pan", value: 0.5 } });
    expect(screen.getByLabelText("Pan, 0.5")).toBeTruthy();
  });
});
