/**
 * AURA Dark — the committed dark cyberpunk console, and the default.
 *
 * Every value here is today's value: the `:root` block of `src/app.css` plus
 * the colour literals the components carried before the sweep. Changing a
 * number in this file changes how AURA looks out of the box; that is what
 * builtins.test.ts pins.
 */

import type { ThemeTokens } from "../tokens";

export const AURA_DARK_TOKENS: ThemeTokens = {
  bgVoid: "#030408",
  bg0: "#05070d",
  bgSunken: "#080a13",
  bg1: "#0a0d17",
  bg2: "#10142a",
  bg3: "#1b2340",
  glass: "#0d111e",

  line: "#6082be",
  edge: "#7aa0dc",

  cyan: "#52e5ff",
  cyanBright: "#8ef0ff",
  cyanDeep: "#1e7f95",
  magenta: "#ff4fd8",
  amber: "#ffc857",
  amberSunken: "#1a1408",
  red: "#ff4152",
  redSoft: "#ff8b96",
  violet: "#9d7bff",
  green: "#5cf2b8",
  orange: "#ff8b5c",

  text: "#d8e3f2",
  textMid: "#8fa3c4",
  textDim: "#5f6c85",
  textFaint: "#39435c",
  textOnAccent: "#ffffff",

  shadow: "#000000",

  trackPalette: ["#52e5ff", "#ff4fd8", "#ffc857", "#9d7bff", "#5cf2b8", "#ff8b5c"],

  borderWidth: "1px",
  focusWidth: "1px",
  glassBlur: "18px",
  glassAlpha: "0.62",
  glowScale: "1",
  bodyGlow: "0.05",
};
