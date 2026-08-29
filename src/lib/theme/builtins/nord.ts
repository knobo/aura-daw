/**
 * Nord — arctic, north-bluish palette.
 */

import type { ThemeTokens } from "../tokens";

export const NORD_TOKENS: ThemeTokens = {
  bgVoid: "#242933",
  bg0: "#2e3440",
  bgSunken: "#242933",
  bg1: "#3b4252",
  bg2: "#434c5e",
  bg3: "#4c566a",
  glass: "#3b4252",

  line: "#4c566a",
  edge: "#616e88",

  cyan: "#88c0d0",
  cyanBright: "#8fbcbb",
  cyanDeep: "#5e81ac",
  magenta: "#b48ead",
  amber: "#ebcb8b",
  amberSunken: "#352d1c",
  red: "#bf616a",
  redSoft: "#d08770",
  violet: "#81a1c1",
  green: "#a3be8c",
  orange: "#d08770",

  text: "#eceff4",
  textMid: "#e5e9f0",
  textDim: "#d8dee9",
  textFaint: "#9aa5b8",
  textOnAccent: "#2e3440",

  shadow: "#000000",

  trackPalette: ["#88c0d0", "#b48ead", "#ebcb8b", "#81a1c1", "#a3be8c", "#d08770"],

  borderWidth: "1px",
  focusWidth: "1px",
  glassBlur: "18px",
  glassAlpha: "0.62",
  glowScale: "1",
  bodyGlow: "0.05",
  // A hint of translucency, in keeping with its soft surfaces.
  panelAlpha: "0.9",

  // material — Nord's flat arctic surfaces take a soft, matte relief.
  bevel: "0.35",
  relief: "0.45",
  sheen: "0.3",
  grain: "0.08",
  // Not brushed: this surface has no direction to it.
  brush: "0",
  ctrlRadius: "6px",
};
