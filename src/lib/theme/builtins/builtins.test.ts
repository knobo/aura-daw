import { describe, expect, it } from "vitest";
import { MATERIAL_KEYS, TOKEN_KEYS } from "../tokens";
import {
  AURA_DARK,
  BUILTIN_BY_ID,
  BUILTIN_THEMES,
  CONSOLE_NOIR,
  DEFAULT_THEME_ID,
  STUDIO_IVORY,
} from "./index";

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
    expect(AURA_DARK.tokens.glassAlpha).toBe("0.62");
    expect(AURA_DARK.tokens.bodyGlow).toBe("0.05");
  });

  // The glow scale is what keeps every call site's designed radius: at 1 the
  // component's own `calc(<radius> * var(--glow-scale))` is the radius it
  // shipped with, so AURA Dark glows exactly as it did before the sweep.
  it("leaves glows at their designed radii", () => {
    expect(AURA_DARK.tokens.glowScale).toBe("1");
  });
});

describe("the flat themes", () => {
  // A theme that turns off the blur must also turn off the transparency:
  // translucent-with-no-blur puts the raw timeline behind panel text.
  it("pair glassBlur: 0px with an opaque panel fill", () => {
    for (const theme of BUILTIN_THEMES) {
      if (theme.tokens.glassBlur === "0px") {
        expect(theme.tokens.glassAlpha, theme.id).toBe("1");
      }
    }
  });
});

describe("the material layer", () => {
  const STRENGTHS = ["bevel", "relief", "sheen", "grain"] as const;

  it("keeps every material strength inside 0..1", () => {
    for (const theme of BUILTIN_THEMES) {
      for (const key of STRENGTHS) {
        const n = Number(theme.tokens[key]);
        expect(Number.isFinite(n), `${theme.id}.${key}`).toBe(true);
        expect(n, `${theme.id}.${key}`).toBeGreaterThanOrEqual(0);
        expect(n, `${theme.id}.${key}`).toBeLessThanOrEqual(1);
      }
    }
  });

  it("gives every built-in a control radius in px", () => {
    for (const theme of BUILTIN_THEMES) {
      expect(theme.tokens.ctrlRadius, theme.id).toMatch(/^\d+(\.\d+)?px$/);
    }
  });

  // The high-contrast themes already zero `glassBlur` and `glowScale`, and
  // the material tokens belong to the same family of decisions: a bevel is a
  // low-contrast cue by construction and grain is literal noise across text.
  // A theme that flattens one and not the other is half-done.
  it("flattens the material wherever glow is switched off for contrast", () => {
    for (const theme of BUILTIN_THEMES) {
      if (!theme.id.startsWith("high-contrast")) continue;
      for (const key of STRENGTHS) {
        expect(theme.tokens[key], `${theme.id}.${key}`).toBe("0");
      }
    }
  });
});

describe("the material themes", () => {
  // These two are the reason the material tokens exist. If their material
  // ever drifts down to the house theme's, they have stopped earning their
  // place in the picker and are just two more palettes.
  it("push the material well past the house theme", () => {
    for (const theme of [CONSOLE_NOIR, STUDIO_IVORY]) {
      expect(Number(theme.tokens.bevel), theme.id).toBeGreaterThan(
        Number(AURA_DARK.tokens.bevel),
      );
      expect(Number(theme.tokens.grain), theme.id).toBeGreaterThan(
        Number(AURA_DARK.tokens.grain),
      );
    }
  });

  // Milled metal is not frosted glass. Both themes make a claim about what
  // the surface is made of, and translucency contradicts it.
  it("are solid, not glass", () => {
    for (const theme of [CONSOLE_NOIR, STUDIO_IVORY]) {
      expect(theme.tokens.glassBlur, theme.id).toBe("0px");
      expect(theme.tokens.glassAlpha, theme.id).toBe("1");
    }
  });

  // Studio Ivory's warm shadow is most of why its cream panels read as an
  // object rather than as holes cut in a page; it is a design decision, not
  // a typo for #000000.
  it("give Studio Ivory a warm shadow rather than a black one", () => {
    expect(STUDIO_IVORY.tokens.shadow).toBe("#3a3227");
  });

  it("run Studio Ivory's surface ramp upward from the panel", () => {
    // On a light theme "more raised" means lighter, not brighter: bg3 must
    // sit above bg2, which must sit above the panel they lie on.
    const hex = (c: string) => parseInt(c.slice(1), 16);
    expect(hex(STUDIO_IVORY.tokens.bg3)).toBeGreaterThan(hex(STUDIO_IVORY.tokens.bg2));
    expect(hex(STUDIO_IVORY.tokens.bg2)).toBeGreaterThan(hex(STUDIO_IVORY.tokens.bg1));
  });

  it("registers both in the picker and keeps the material keys complete", () => {
    for (const theme of [CONSOLE_NOIR, STUDIO_IVORY]) {
      expect(BUILTIN_THEMES, theme.id).toContain(theme);
      for (const key of MATERIAL_KEYS) {
        expect(theme.tokens[key], `${theme.id}.${key}`).toBeDefined();
      }
    }
  });
});
