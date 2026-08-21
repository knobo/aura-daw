/**
 * Gruvbox Dark — retro groove warm palette by Pavel Pertsev.
 */

import type { ThemeTokens } from "../tokens";

export const GRUVBOX_DARK_TOKENS: ThemeTokens = {
  bgVoid: "#1d2021",
  bg0: "#282828",
  bgSunken: "#1d2021",
  bg1: "#32302f",
  bg2: "#3c3836",
  bg3: "#504945",
  glass: "#32302f",

  line: "#504945",
  edge: "#665c54",

  cyan: "#8ec07c",
  cyanBright: "#a9b665",
  cyanDeep: "#458588",
  magenta: "#d3869b",
  amber: "#fabd2f",
  amberSunken: "#332709",
  red: "#fb4934",
  redSoft: "#ea6962",
  violet: "#ba6b8f",
  green: "#b8bb26",
  orange: "#fe8019",

  text: "#ebdbb2",
  textMid: "#d5c4a1",
  textDim: "#bdae93",
  textFaint: "#928374",
  textOnAccent: "#282828",

  shadow: "#000000",

  trackPalette: ["#8ec07c", "#d3869b", "#fabd2f", "#ba6b8f", "#b8bb26", "#fe8019"],

  borderWidth: "1px",
  focusWidth: "1px",
  glassBlur: "18px",
  glassAlpha: "0.62",
  glowScale: "1",
  bodyGlow: "0.05",

  // material — Gruvbox is retro and warm, so it takes the grainiest surface of the
  // borrowed palettes and the hardest corners.
  bevel: "0.45",
  relief: "0.5",
  sheen: "0.28",
  grain: "0.16",
  ctrlRadius: "4px",
};
