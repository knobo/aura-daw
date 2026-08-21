/**
 * Shared param formatting/naming, extracted byte-identical from
 * `PluginParamPanel.svelte`'s private `fmt`/`unitOf`/group-splitting so the
 * chips, the matrix and the fader (Tasks 2-4) render exactly the same text.
 */
import { describe, expect, it } from "vitest";
import type { PluginParamInfo } from "../types/ipc";
import {
  formatParamDisplay,
  formatParamValue,
  paramGroupName,
  paramNormalized,
  paramUnit,
  shortParamName,
} from "./plugin-params";

function param(over: Partial<PluginParamInfo> = {}): PluginParamInfo {
  return { id: 1, name: "Cutoff", min: 0, max: 1, default: 0.5, value: 0.5, steps: 0, ...over };
}

describe("shortParamName", () => {
  it("splits on ' / ' and returns the name half", () => {
    expect(shortParamName("Filter / Cutoff")).toBe("Cutoff");
  });
  it("returns the name unchanged when there is no ' / '", () => {
    expect(shortParamName("Cutoff")).toBe("Cutoff");
  });
});

describe("paramGroupName", () => {
  it("returns the group half of a 'Group / Name' name", () => {
    expect(paramGroupName("Filter / Cutoff")).toBe("Filter");
  });
  it("falls back to 'parameters' when there is no ' / '", () => {
    expect(paramGroupName("Cutoff")).toBe("parameters");
  });
});

describe("formatParamValue / paramUnit", () => {
  it("rounds stepped params to an integer with no unit", () => {
    const p = param({ name: "Waveform", min: 0, max: 3, steps: 4, value: 2.4 });
    expect(formatParamValue(p)).toBe("2");
    expect(paramUnit(p)).toBe("");
  });

  it("cutoff/freq switch to k above 1000 and carry unit hz", () => {
    const p = param({ name: "Filter / Cutoff", min: 20, max: 20000, value: 1500 });
    expect(formatParamValue(p)).toBe("1.50k");
    expect(paramUnit(p)).toBe("hz");
    const low = param({ name: "LFO Freq", min: 0, max: 20000, value: 440 });
    expect(formatParamValue(low)).toBe("440");
    expect(paramUnit(low)).toBe("hz");
  });

  it("a 0..1 range shows two decimals", () => {
    const p = param({ name: "Mix", min: 0, max: 1, value: 0.333 });
    expect(formatParamValue(p)).toBe("0.33");
  });

  it("detune carries unit ct", () => {
    const p = param({ name: "Osc / Detune", min: -100, max: 100, value: 5 });
    expect(paramUnit(p)).toBe("ct");
  });

  it("attack/release/decay carry unit s", () => {
    for (const name of ["Attack", "Release", "Decay"]) {
      expect(paramUnit(param({ name, min: 0, max: 5, value: 0.2 }))).toBe("s");
    }
  });

  it("|value| >= 100 drops the decimal", () => {
    const p = param({ name: "Gain", min: -200, max: 200, value: 150.7 });
    expect(formatParamValue(p)).toBe("151");
  });

  it("|value| < 100 outside 0..1 range keeps one decimal", () => {
    const p = param({ name: "Gain", min: -200, max: 200, value: 42.3 });
    expect(formatParamValue(p)).toBe("42.3");
  });

  it("uses the passed value override instead of p.value", () => {
    const p = param({ name: "Mix", min: 0, max: 1, value: 0.1 });
    expect(formatParamValue(p, 0.75)).toBe("0.75");
  });
});

describe("formatParamDisplay", () => {
  it("concatenates value and unit", () => {
    const p = param({ name: "Filter / Cutoff", min: 20, max: 20000, value: 1500 });
    expect(formatParamDisplay(p)).toBe("1.50khz");
  });
});

describe("paramNormalized", () => {
  it("returns the 0..1 position of value in the param's range", () => {
    const p = param({ min: 0, max: 200, value: 50 });
    expect(paramNormalized(p)).toBe(0.25);
  });
  it("returns 0 when min === max instead of dividing by zero", () => {
    const p = param({ min: 5, max: 5, value: 5 });
    expect(paramNormalized(p)).toBe(0);
  });
  it("uses the passed value override instead of p.value", () => {
    const p = param({ min: 0, max: 200, value: 50 });
    expect(paramNormalized(p, 100)).toBe(0.5);
  });
});
