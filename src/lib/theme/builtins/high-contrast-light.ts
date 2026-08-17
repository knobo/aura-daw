/**
 * High Contrast Light — the same affordances on a white ground, for people
 * who read better on light backgrounds and for screens in daylight.
 */

import type { ThemeTokens } from "../tokens";

export const HIGH_CONTRAST_LIGHT_TOKENS: ThemeTokens = {
  bgVoid: "#ffffff",
  bg0: "#ffffff",
  bgSunken: "#f2f2f2",
  bg1: "#fafafa",
  bg2: "#ebebeb",
  bg3: "#dcdcdc",
  glass: "#fafafa",

  line: "#3d3d3d",
  edge: "#1a1a1a",

  cyan: "#005f73",
  cyanBright: "#0a7f96",
  cyanDeep: "#003b47",
  magenta: "#9b0079",
  amber: "#7a4b00",
  amberSunken: "#f5e6c8",
  red: "#b3001b",
  redSoft: "#8a0015",
  violet: "#4b2ba8",
  green: "#0a6b45",
  orange: "#8a3b00",

  text: "#000000",
  textMid: "#262626",
  textDim: "#3d3d3d",
  textFaint: "#565656",
  textOnAccent: "#ffffff",

  shadow: "#000000",

  trackPalette: ["#005f73", "#9b0079", "#7a4b00", "#4b2ba8", "#0a6b45", "#8a3b00"],

  borderWidth: "2px",
  focusWidth: "3px",
  glassBlur: "0px",
  glowBlur: "0px",
  bodyGlow: "0",
};
