/**
 * Solarized Dark — Ethan Schoonover's precision palette.
 */

import type { ThemeTokens } from "../tokens";

export const SOLARIZED_DARK_TOKENS: ThemeTokens = {
  bgVoid: "#00212b",
  bg0: "#002b36",
  bgSunken: "#00212b",
  bg1: "#073642",
  bg2: "#0a4352",
  bg3: "#0e5263",
  glass: "#073642",

  line: "#586e75",
  edge: "#657b83",

  cyan: "#2aa198",
  cyanBright: "#3dc0b6",
  cyanDeep: "#1c736c",
  magenta: "#e04693",
  amber: "#b58900",
  amberSunken: "#2b2000",
  red: "#dc322f",
  redSoft: "#e55f5c",
  violet: "#7b80d5",
  green: "#859900",
  orange: "#d85620",

  text: "#93a1a1",
  textMid: "#839496",
  textDim: "#7c8e91",
  textFaint: "#586e75",
  textOnAccent: "#002b36",

  shadow: "#000000",

  trackPalette: ["#2aa198", "#e04693", "#b58900", "#7b80d5", "#859900", "#d85620"],

  borderWidth: "1px",
  focusWidth: "1px",
  glassBlur: "18px",
  glassAlpha: "0.62",
  glowScale: "1",
  bodyGlow: "0.05",
  // Barely translucent — Solarized's surfaces are close in value,
  // so a see-through panel muddies more than it reveals.
  panelAlpha: "0.92",

  // material — Solarized is a deliberately low-contrast palette, so its material stays
  // quiet enough not to fight it.
  bevel: "0.3",
  relief: "0.4",
  sheen: "0.25",
  grain: "0.06",
  ctrlRadius: "5px",
};
