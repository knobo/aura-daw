/**
 * Preference schema: every preference the app exposes is declared once in
 * PREF_SCHEMA (kind, default, constraints, UI copy). These tests pin the
 * contract the rest of the app builds on: defaults are valid under their own
 * coercion, and coercePref() turns arbitrary disk junk into either a valid
 * value or undefined — never a crash, never an out-of-range value.
 */

import { describe, expect, it } from "vitest";
import {
  PREF_CATEGORIES,
  PREF_SCHEMA,
  TOLERANCE_CENTS,
  coercePref,
  schemaDefaults,
  type PrefId,
} from "./schema";

const ids = Object.keys(PREF_SCHEMA) as PrefId[];

describe("PREF_SCHEMA integrity", () => {
  it("every default survives its own coercion unchanged", () => {
    for (const id of ids) {
      const def = PREF_SCHEMA[id];
      // toEqual, not toBe: pathList coercion always builds a fresh array
      // (de-dupe via Set), so an array default is value-equal, not identical.
      expect(coercePref(def, def.default), id).toEqual(def.default);
    }
  });

  it("every preference belongs to a declared category", () => {
    const cats = new Set(PREF_CATEGORIES.map((c) => c.id));
    for (const id of ids) expect(cats.has(PREF_SCHEMA[id].category), id).toBe(true);
  });

  it("schemaDefaults() covers exactly the schema ids", () => {
    expect(Object.keys(schemaDefaults()).sort()).toEqual([...ids].sort());
  });

  it("declares the launch set", () => {
    expect(ids).toEqual(
      expect.arrayContaining([
        "clipOpenAutoplay",
        "noteFlash",
        "uiZoom",
        "mcpDefaultMode",
        "metronome",
        "countInBars",
        "pluginGuiOnTop",
      ]),
    );
    expect(PREF_SCHEMA.clipOpenAutoplay.default).toBe(false);
    expect(PREF_SCHEMA.mcpDefaultMode.default).toBe("confirmDestructive");
    expect(PREF_SCHEMA.pluginGuiOnTop.default).toBe(true);
    expect(PREF_SCHEMA.pluginGuiOnTop.category).toBe("interface");
  });

  it("declares how many recent projects the menu shows", () => {
    const def = PREF_SCHEMA.recentProjectsMax;
    expect(def.kind).toBe("number");
    expect(def.category).toBe("interface");
    expect(def.default).toBe(8);
    expect(def.min).toBe(1);
    expect(def.max).toBe(20);
    expect(def.step).toBe(1);
  });

  it("declares the pitch coach preferences with usable defaults", () => {
    expect(PREF_SCHEMA.pitchTolerance.default).toBe("standard");
    expect(PREF_SCHEMA.pitchLatencyOffsetMs.default).toBe(0);
    expect(PREF_SCHEMA.pitchLaneFollow.default).toBe("fixed");
    expect(PREF_SCHEMA.pitchTrailSnap.default).toBe(false);
    expect(PREF_SCHEMA.pitchRehearseKey.default).toBe("h");
  });

  it("maps every tolerance option to a cents value", () => {
    const ids = PREF_SCHEMA.pitchTolerance.options.map((o) => o.value);
    expect(ids).toEqual(["forgiving", "standard", "strict", "pro"]);
    for (const id of ids) expect(TOLERANCE_CENTS[id]).toBeGreaterThan(0);
    expect(TOLERANCE_CENTS.standard).toBe(50);
  });

  it("lets the latency offset run negative, since the correction can go either way", () => {
    const def = PREF_SCHEMA.pitchLatencyOffsetMs;
    expect(def.min).toBe(-50);
    expect(def.max).toBe(50);
    expect(coercePref(def, -37)).toBe(-37);
    expect(coercePref(def, -900)).toBe(-50);
  });

  it("offers OFF as a rehearse key, so the hold can be disabled entirely", () => {
    const values = PREF_SCHEMA.pitchRehearseKey.options.map((o) => o.value);
    expect(values).toContain("none");
    expect(coercePref(PREF_SCHEMA.pitchRehearseKey, "none")).toBe("none");
  });

  it("browserAudition is a boolean that defaults off", () => {
    const def = PREF_SCHEMA.browserAudition;
    expect(def.kind).toBe("boolean");
    // Default OFF is the whole point: a browser that makes noise you did
    // not ask for is the fastest way to lose someone (design §8.2).
    expect(def.default).toBe(false);
    expect(def.category).toBe("library");
  });
});

describe("coercePref", () => {
  it("boolean: accepts only real booleans", () => {
    const def = PREF_SCHEMA.clipOpenAutoplay;
    expect(coercePref(def, true)).toBe(true);
    expect(coercePref(def, false)).toBe(false);
    for (const junk of [1, 0, "true", null, undefined, {}]) {
      expect(coercePref(def, junk)).toBeUndefined();
    }
  });

  it("enum: accepts only declared option values", () => {
    const def = PREF_SCHEMA.mcpDefaultMode;
    expect(coercePref(def, "readOnly")).toBe("readOnly");
    expect(coercePref(def, "full")).toBe("full");
    for (const junk of ["FULL", "yolo", 2, null, undefined]) {
      expect(coercePref(def, junk)).toBeUndefined();
    }
  });

  it("number: clamps to [min, max] and snaps to the step grid", () => {
    const def = PREF_SCHEMA.uiZoom; // 0.8 .. 2.0 step 0.1
    expect(coercePref(def, 1.5)).toBe(1.5);
    expect(coercePref(def, 9)).toBe(2.0);
    expect(coercePref(def, 0.1)).toBe(0.8);
    expect(coercePref(def, 1.4499999)).toBe(1.4);
    expect(coercePref(def, 1.25)).toBe(1.3); // ties round up, matching old zoom snap
    for (const junk of [NaN, Infinity, "1.2", null, undefined]) {
      expect(coercePref(def, junk)).toBeUndefined();
    }
  });
});

describe("pathList preferences", () => {
  it("declares librarySampleFolders as an empty pathList in the library category", () => {
    const def = PREF_SCHEMA.librarySampleFolders;
    expect(def.kind).toBe("pathList");
    expect(def.default).toEqual([]);
    expect(def.category).toBe("library");
    expect(PREF_CATEGORIES.map((c) => c.id)).toContain("library");
  });

  it("coerces a pathList: keeps strings, drops junk, de-dupes, preserves order", () => {
    const def = PREF_SCHEMA.librarySampleFolders;
    expect(coercePref(def, ["/a", "/b", "/a", "", 7, null, "/c"])).toEqual(["/a", "/b", "/c"]);
  });

  it("rejects a non-array pathList value whole", () => {
    const def = PREF_SCHEMA.librarySampleFolders;
    expect(coercePref(def, "/a")).toBeUndefined();
    expect(coercePref(def, 3)).toBeUndefined();
    expect(coercePref(def, null)).toBeUndefined();
  });

  it("hands out a FRESH array from schemaDefaults so the schema default cannot be mutated", () => {
    const a = schemaDefaults().librarySampleFolders;
    const b = schemaDefaults().librarySampleFolders;
    expect(a).toEqual([]);
    expect(a).not.toBe(b);
    expect(a).not.toBe(PREF_SCHEMA.librarySampleFolders.default);
  });
});

describe("the choice kind (runtime-catalogued options)", () => {
  const def = PREF_SCHEMA.theme;

  it("declares the theme preference against the themes catalog", () => {
    expect(def.kind).toBe("choice");
    expect(def.default).toBe("aura-dark");
    expect(def.category).toBe("interface");
    if (def.kind === "choice") expect(def.catalog).toBe("themes");
  });

  it("accepts any non-empty string, because the catalog is not known here", () => {
    expect(coercePref(def, "solarized-dark")).toBe("solarized-dark");
    expect(coercePref(def, "a-theme-someone-invented")).toBe("a-theme-someone-invented");
  });

  it("rejects blanks and non-strings", () => {
    expect(coercePref(def, "")).toBeUndefined();
    expect(coercePref(def, "   ")).toBeUndefined();
    expect(coercePref(def, 7)).toBeUndefined();
    expect(coercePref(def, null)).toBeUndefined();
  });

  it("leaves literal-union preferences resolving to enum defs", () => {
    // countInBars is "0"|"1"|"2"|"4" — a union, so it must still be an enum
    // with a static options list, not a choice.
    expect(PREF_SCHEMA.countInBars.kind).toBe("enum");
  });
});


/**
 * The meter face is the one preference whose options are also a list inside a
 * component: Gauge.svelte branches on each of them, and a value the schema
 * offers but the component has no branch for renders as an empty square in a
 * channel strip with nothing anywhere saying why.
 */
describe("the meter face", () => {
  const def = PREF_SCHEMA.meterFace;

  it("offers exactly the four faces Gauge.svelte draws", () => {
    expect(def.kind).toBe("enum");
    expect(def.kind === "enum" && def.options.map((o) => o.value)).toEqual([
      "led",
      "dial",
      "ladder",
      "needle",
    ]);
  });

  // LED ARC is the face that matches the knob's dot ring, which is the whole
  // reason the surface reads as one instrument. It is the default on purpose.
  it("defaults to the face that matches the knobs", () => {
    expect(def.default).toBe("led");
  });

  it("rejects a face that does not exist rather than storing it", () => {
    expect(coercePref(def, "vu-plus")).toBeUndefined();
  });
});

/**
 * The strip keys are the same shape of contract as the meter face: an option
 * the schema offers that Lamp.svelte has no branch for renders as an
 * unstyled button in a channel strip.
 */
describe("the strip keys", () => {
  const def = PREF_SCHEMA.stripKeys;

  it("offers exactly the two shapes Lamp.svelte draws", () => {
    expect(def.kind).toBe("enum");
    expect(def.kind === "enum" && def.options.map((o) => o.value)).toEqual(["key", "bar"]);
  });

  it("defaults to the moulded keys", () => {
    expect(def.default).toBe("key");
  });
});
