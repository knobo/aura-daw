/**
 * The feature's actual acceptance criterion. Themes exist here because some
 * people cannot read the default one; a theme that ships below AA has not
 * done its job, and taste is not a defence.
 */
import { describe, expect, it } from "vitest";
import { BUILTIN_THEMES } from "./builtins/index";
import { rgbTriple } from "./tokens";

/** WCAG 2.1 relative luminance. */
function luminance(color: string): number {
  const [r, g, b] = rgbTriple(color).split(" ").map(Number);
  const lin = (c: number) => {
    const s = c / 255;
    return s <= 0.03928 ? s / 12.92 : ((s + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b);
}

/** WCAG contrast ratio, 1..21. */
function ratio(a: string, b: string): number {
  const [hi, lo] = [luminance(a), luminance(b)].sort((x, y) => y - x);
  return (hi + 0.05) / (lo + 0.05);
}

describe("every built-in theme is readable", () => {
  for (const theme of BUILTIN_THEMES) {
    it(`${theme.id}: body text clears AA on the panel surface`, () => {
      expect(ratio(theme.tokens.text, theme.tokens.bg1)).toBeGreaterThanOrEqual(4.5);
    });

    it(`${theme.id}: secondary text clears AA large on the panel surface`, () => {
      expect(ratio(theme.tokens.textDim, theme.tokens.bg1)).toBeGreaterThanOrEqual(3);
    });

    it(`${theme.id}: every track colour is distinguishable from the lane`, () => {
      for (const [i, c] of theme.tokens.trackPalette.entries()) {
        expect(ratio(c, theme.tokens.bg1), `track ${i + 1}`).toBeGreaterThanOrEqual(3);
      }
    });
  }
});

describe("the high-contrast themes clear AAA", () => {
  for (const id of ["high-contrast-dark", "high-contrast-light"]) {
    const theme = BUILTIN_THEMES.find((t) => t.id === id)!;

    it(`${id}: body text clears AAA`, () => {
      expect(ratio(theme.tokens.text, theme.tokens.bg1)).toBeGreaterThanOrEqual(7);
    });

    it(`${id}: secondary text also clears AA, so nothing fades out`, () => {
      expect(ratio(theme.tokens.textDim, theme.tokens.bg1)).toBeGreaterThanOrEqual(4.5);
    });

    it(`${id}: turns off glass, glow and the body wash, and thickens edges`, () => {
      expect(theme.tokens.glassBlur).toBe("0px");
      // Opaque, not merely unblurred: a translucent panel with no frosting
      // puts the timeline grid straight through its own text.
      expect(theme.tokens.glassAlpha).toBe("1");
      expect(theme.tokens.glowScale).toBe("0");
      expect(theme.tokens.bodyGlow).toBe("0");
      expect(theme.tokens.borderWidth).toBe("2px");
      expect(theme.tokens.focusWidth).toBe("3px");
    });
  }
});
