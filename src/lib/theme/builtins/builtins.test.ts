import { describe, expect, it } from "vitest";
import { TOKEN_KEYS } from "../tokens";
import { AURA_DARK, BUILTIN_BY_ID, BUILTIN_THEMES, DEFAULT_THEME_ID } from "./index";

describe("the built-in registry", () => {
  it("defaults to aura-dark, and aura-dark is registered first", () => {
    expect(DEFAULT_THEME_ID).toBe("aura-dark");
    expect(BUILTIN_THEMES[0].id).toBe("aura-dark");
    expect(BUILTIN_BY_ID["aura-dark"]).toBe(AURA_DARK);
  });

  it("gives every built-in a value for every token key", () => {
    for (const theme of BUILTIN_THEMES) {
      for (const key of TOKEN_KEYS) {
        expect(theme.tokens[key], `${theme.id} is missing ${key}`).toBeDefined();
      }
    }
  });

  it("gives every built-in exactly six track colours", () => {
    for (const theme of BUILTIN_THEMES) {
      expect(theme.tokens.trackPalette, theme.id).toHaveLength(6);
    }
  });

  it("has unique ids", () => {
    const ids = BUILTIN_THEMES.map((t) => t.id);
    expect(new Set(ids).size).toBe(ids.length);
  });

  it("marks every built-in as a builtin extending itself", () => {
    for (const theme of BUILTIN_THEMES) {
      expect(theme.source).toBe("builtin");
      expect(theme.base).toBe(theme.id);
    }
  });
});

describe("AURA Dark", () => {
  // These are the values in src/app.css :root today. If this test fails, the
  // default theme is about to change appearance — which the plan forbids.
  it("reproduces today's app.css surface and accent tokens exactly", () => {
    expect(AURA_DARK.tokens.bg0).toBe("#05070d");
    expect(AURA_DARK.tokens.bg1).toBe("#0a0d17");
    expect(AURA_DARK.tokens.bg2).toBe("#10142a");
    expect(AURA_DARK.tokens.cyan).toBe("#52e5ff");
    expect(AURA_DARK.tokens.magenta).toBe("#ff4fd8");
    expect(AURA_DARK.tokens.red).toBe("#ff4152");
    expect(AURA_DARK.tokens.amber).toBe("#ffc857");
    expect(AURA_DARK.tokens.violet).toBe("#9d7bff");
    expect(AURA_DARK.tokens.text).toBe("#d8e3f2");
    expect(AURA_DARK.tokens.textDim).toBe("#5f6c85");
    expect(AURA_DARK.tokens.textFaint).toBe("#39435c");
  });

  it("keeps the existing TRACK_PALETTE order", () => {
    expect(AURA_DARK.tokens.trackPalette).toEqual([
      "#52e5ff", "#ff4fd8", "#ffc857", "#9d7bff", "#5cf2b8", "#ff8b5c",
    ]);
  });

  it("keeps today's affordances", () => {
    expect(AURA_DARK.tokens.borderWidth).toBe("1px");
    expect(AURA_DARK.tokens.focusWidth).toBe("1px");
    expect(AURA_DARK.tokens.glassBlur).toBe("18px");
    expect(AURA_DARK.tokens.glowBlur).toBe("6px");
    expect(AURA_DARK.tokens.bodyGlow).toBe("0.05");
  });
});
