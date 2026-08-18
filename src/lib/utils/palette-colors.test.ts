/**
 * The palette ramp. The point of these is that the ramp is made of THEME
 * TOKENS, so it survives a light theme, a high-contrast theme and a user theme
 * — a hardcoded colour here would break exactly the people the Composer is for.
 */
import { describe, expect, it } from "vitest";
import { BUILTIN_THEMES } from "../theme/builtins/index";
import { roleGloss, roleInk } from "./palette-colors";
import type { NoteRole } from "../types/ipc";

const ROLES: NoteRole[] = [
  "root",
  "third",
  "fifth",
  "seventh",
  "extension",
  "scale",
  "tension",
  "avoid",
  "outside",
];

describe("roleInk", () => {
  it("returns a token from the active theme for every role, in every theme", () => {
    for (const theme of BUILTIN_THEMES) {
      const values = new Set(
        Object.entries(theme.tokens)
          .filter(([, v]) => typeof v === "string")
          .map(([, v]) => v as string),
      );
      for (const role of ROLES) {
        const ink = roleInk(role, theme.tokens);
        expect(values.has(ink.color), `${theme.id}/${role}: ${ink.color}`).toBe(true);
      }
    }
  });

  it("paints chord tones more strongly than colour, and colour more than nothing", () => {
    const t = BUILTIN_THEMES[0].tokens;
    expect(roleInk("root", t).keyAlpha).toBeGreaterThan(roleInk("extension", t).keyAlpha);
    expect(roleInk("extension", t).keyAlpha).toBeGreaterThan(roleInk("outside", t).keyAlpha);
    expect(roleInk("outside", t).rowAlpha).toBe(0);
  });

  it("keeps every alpha inside a usable range — a tint, never a wash", () => {
    const t = BUILTIN_THEMES[0].tokens;
    for (const role of ROLES) {
      const ink = roleInk(role, t);
      expect(ink.rowAlpha).toBeGreaterThanOrEqual(0);
      expect(ink.rowAlpha).toBeLessThanOrEqual(0.2);
      expect(ink.keyAlpha).toBeGreaterThanOrEqual(0);
      expect(ink.keyAlpha).toBeLessThanOrEqual(0.6);
    }
  });

  it("separates the answer from the warning: root and avoid never share a colour", () => {
    for (const theme of BUILTIN_THEMES) {
      expect(roleInk("root", theme.tokens).color).not.toBe(roleInk("avoid", theme.tokens).color);
      expect(roleInk("third", theme.tokens).color).not.toBe(roleInk("avoid", theme.tokens).color);
    }
  });
});

describe("roleGloss", () => {
  it("says what each role means without a theory word standing alone", () => {
    for (const role of ROLES) {
      expect(roleGloss(role).length).toBeGreaterThan(3);
    }
    expect(roleGloss("avoid")).toContain("fights");
    expect(roleGloss("third")).toContain("major or minor");
    expect(roleGloss("tension")).toContain("resolve");
  });
});
