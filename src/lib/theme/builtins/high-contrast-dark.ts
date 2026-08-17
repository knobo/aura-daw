/**
 * High Contrast Dark — the reason this feature exists.
 *
 * Pure black ground, near-white text, saturated accents, and every
 * affordance turned up: no frosted glass, no glow, no background wash,
 * double-thick borders and a focus ring you cannot miss. The text ramp is
 * compressed so "dim" and "faint" labels stay readable instead of fading
 * into the panel — the single change that does the most work here.
 */

import type { ThemeTokens } from "../tokens";

export const HIGH_CONTRAST_DARK_TOKENS: ThemeTokens = {
  bgVoid: "#000000",
  bg0: "#000000",
  bgSunken: "#000000",
  bg1: "#0b0b0b",
  bg2: "#1a1a1a",
  bg3: "#2b2b2b",
  glass: "#0b0b0b",

  line: "#c9c9c9",
  edge: "#e6e6e6",

  cyan: "#5ce1ff",
  cyanBright: "#a6efff",
  cyanDeep: "#0b6f88",
  magenta: "#ff7ae0",
  amber: "#ffd166",
  amberSunken: "#2b2000",
  red: "#ff6b78",
  redSoft: "#ffa8b0",
  violet: "#b79cff",
  green: "#5cf2b8",
  orange: "#ffa06b",

  text: "#ffffff",
  textMid: "#e0e0e0",
  textDim: "#c2c2c2",
  textFaint: "#a3a3a3",
  textOnAccent: "#000000",

  shadow: "#000000",

  trackPalette: ["#5ce1ff", "#ff7ae0", "#ffd166", "#b79cff", "#5cf2b8", "#ffa06b"],

  borderWidth: "2px",
  focusWidth: "3px",
  glassBlur: "0px",
  glassAlpha: "1",
  glowScale: "0",
  bodyGlow: "0",
};
