/**
 * Tempo / meter helpers used by the transport-bar editor.
 *
 * The production change that would fail these: clamp/parse no longer
 * pinning 40–300, or bar length ignoring the denominator (6/8 collapsing
 * to six quarter-notes).
 */

import { describe, expect, it } from "vitest";
import {
  clampTempo,
  parseTempo,
  isValidMeter,
  quartersPerBar,
  TEMPO_MIN,
  TEMPO_MAX,
} from "./tempo";

describe("clampTempo", () => {
  it("pins the engine's 40–300 range and one decimal", () => {
    expect(clampTempo(128)).toBe(128);
    expect(clampTempo(128.46)).toBe(128.5);
    expect(clampTempo(10)).toBe(TEMPO_MIN);
    expect(clampTempo(400)).toBe(TEMPO_MAX);
  });

  it("falls back to 120 when the value is not a number", () => {
    expect(clampTempo(Number.NaN)).toBe(120);
    expect(clampTempo(Number.POSITIVE_INFINITY)).toBe(120);
  });
});

describe("parseTempo", () => {
  it("reads a typed bpm and clamps it", () => {
    expect(parseTempo("128")).toBe(128);
    expect(parseTempo(" 90.2 ")).toBe(90.2);
    expect(parseTempo("12")).toBe(TEMPO_MIN);
  });

  it("rejects empty or non-numeric input", () => {
    expect(parseTempo("")).toBeNull();
    expect(parseTempo("abc")).toBeNull();
  });
});

describe("isValidMeter", () => {
  it("accepts common DAW signatures", () => {
    expect(isValidMeter(4, 4)).toBe(true);
    expect(isValidMeter(3, 4)).toBe(true);
    expect(isValidMeter(6, 8)).toBe(true);
    expect(isValidMeter(7, 8)).toBe(true);
  });

  it("rejects zero, non-integer, or exotic denominators", () => {
    expect(isValidMeter(0, 4)).toBe(false);
    expect(isValidMeter(4, 3)).toBe(false);
    expect(isValidMeter(4.5, 4)).toBe(false);
  });
});

describe("quartersPerBar", () => {
  it("is the numerator at /4 and half that at /8", () => {
    expect(quartersPerBar(4, 4)).toBe(4);
    expect(quartersPerBar(3, 4)).toBe(3);
    expect(quartersPerBar(6, 8)).toBe(3);
    expect(quartersPerBar(7, 8)).toBe(3.5);
  });
});
