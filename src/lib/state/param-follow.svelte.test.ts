/**
 * The driven-param read-back store. Two things matter here and neither is
 * arithmetic: that the store repeats exactly what the engine reported, and
 * that it stays quiet when nothing changed — it is fed from the 60 Hz meter
 * callback for the app's whole lifetime, so a store that reassigned on every
 * frame would invalidate its readers 60 times a second while a project sits
 * idle.
 */
import { beforeEach, describe, expect, it } from "vitest";
import { paramFollow } from "./param-follow.svelte";
import type { DrivenParam } from "../types/ipc";

function driven(instanceId: string, index: number, value: number): DrivenParam {
  return { instanceId, index, value };
}

beforeEach(() => {
  paramFollow.reset();
});

describe("paramFollow", () => {
  it("reports the value the engine said it wrote, per instance and param", () => {
    paramFollow.apply([driven("i1", 7, 0.25), driven("i2", 7, 0.9)]);
    expect(paramFollow.valueFor("i1", 7)).toBe(0.25);
    expect(paramFollow.valueFor("i2", 7)).toBe(0.9);
    expect(paramFollow.active).toBe(true);
  });

  it("says nothing about a param no lane is driving", () => {
    paramFollow.apply([driven("i1", 7, 0.25)]);
    expect(paramFollow.valueFor("i1", 8)).toBeUndefined();
    expect(paramFollow.valueFor("i9", 7)).toBeUndefined();
  });

  it("an empty frame clears the set — a stopped transport hands the knob back", () => {
    paramFollow.apply([driven("i1", 7, 0.25)]);
    paramFollow.apply([]);
    expect(paramFollow.active).toBe(false);
    expect(paramFollow.valueFor("i1", 7)).toBeUndefined();
  });

  it("treats a backend with no drivenParams field as driving nothing", () => {
    paramFollow.apply([driven("i1", 7, 0.25)]);
    paramFollow.apply(undefined);
    expect(paramFollow.active).toBe(false);
  });

  it("does not reassign when the frame repeats the held set", () => {
    paramFollow.apply([driven("i1", 7, 0.25), driven("i1", 8, 0.5)]);
    const held = paramFollow.driven;
    // Order is the engine's, not ours: same pairs, other sequence.
    paramFollow.apply([driven("i1", 8, 0.5), driven("i1", 7, 0.25)]);
    expect(paramFollow.driven).toBe(held);
  });

  it("stays quiet frame after frame while nothing is automated", () => {
    const held = paramFollow.driven;
    paramFollow.apply([]);
    paramFollow.apply(undefined);
    paramFollow.apply([]);
    expect(paramFollow.driven).toBe(held);
  });

  it("reassigns as soon as a value moves, however slightly", () => {
    paramFollow.apply([driven("i1", 7, 0.25)]);
    const held = paramFollow.driven;
    paramFollow.apply([driven("i1", 7, 0.2500001)]);
    expect(paramFollow.driven).not.toBe(held);
    expect(paramFollow.valueFor("i1", 7)).toBe(0.2500001);
  });

  it("reassigns when a param joins or leaves at the same count", () => {
    paramFollow.apply([driven("i1", 7, 0.25)]);
    const held = paramFollow.driven;
    paramFollow.apply([driven("i1", 8, 0.25)]);
    expect(paramFollow.driven).not.toBe(held);
    expect(paramFollow.valueFor("i1", 7)).toBeUndefined();
    expect(paramFollow.valueFor("i1", 8)).toBe(0.25);
  });
});
