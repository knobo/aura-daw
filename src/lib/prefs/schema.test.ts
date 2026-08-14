/**
 * Preference schema: every preference the app exposes is declared once in
 * PREF_SCHEMA (kind, default, constraints, UI copy). These tests pin the
 * contract the rest of the app builds on: defaults are valid under their own
 * coercion, and coercePref() turns arbitrary disk junk into either a valid
 * value or undefined — never a crash, never an out-of-range value.
 */

import { describe, expect, it } from "vitest";
import { PREF_CATEGORIES, PREF_SCHEMA, coercePref, schemaDefaults, type PrefId } from "./schema";

const ids = Object.keys(PREF_SCHEMA) as PrefId[];

describe("PREF_SCHEMA integrity", () => {
  it("every default survives its own coercion unchanged", () => {
    for (const id of ids) {
      const def = PREF_SCHEMA[id];
      expect(coercePref(def, def.default), id).toBe(def.default);
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
      expect.arrayContaining(["clipOpenAutoplay", "noteFlash", "uiZoom", "mcpDefaultMode"]),
    );
    expect(PREF_SCHEMA.clipOpenAutoplay.default).toBe(false);
    expect(PREF_SCHEMA.mcpDefaultMode.default).toBe("confirmDestructive");
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
