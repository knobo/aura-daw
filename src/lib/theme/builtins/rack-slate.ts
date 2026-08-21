/**
 * Rack Slate — cool steel modules on a dark gutter, lit by one orange lamp.
 *
 * The idiom is the modern software-synth front panel: the window is not one
 * flat surface but a grid of separate MODULE blocks, each a gently
 * top-lit face floating in a near-black gutter, each wearing its name on a
 * small tab at its top-left. That layout language is what `.module` in
 * app.css implements; this theme is the palette tuned to make it read.
 *
 * Three decisions carry it:
 *
 * - **The ground is a gutter, not a panel.** `bg0` is markedly darker than
 *   `bg1`, so the gaps between modules read as shadow between real objects.
 *   On AURA Dark those two are nearly the same value and the same markup
 *   reads as boxes drawn on a page.
 * - **Sheen is high (0.85).** The vertical gradient down each module face is
 *   the single strongest "this is a physical panel" cue, well above bevel or
 *   grain — a flat fill with a crisp edge still looks like a div.
 * - **One accent, used sparingly.** Orange on cool slate is the whole colour
 *   story: selection, active values, meters. Everything else is grey. A
 *   second saturated accent competing with it is what makes a panel look
 *   like a website.
 *
 * `panelAlpha` is `1` — the dock is a wall here, which is what makes docking
 * it beside the timeline (rather than floating it over) look deliberate.
 */

import type { ThemeTokens } from "../tokens";

export const RACK_SLATE_TOKENS: ThemeTokens = {
  // The gutter behind and between the modules.
  bgVoid: "#1c1f27",
  bg0: "#23272f",
  bgSunken: "#2a2f39",
  // The module face. Everything raised steps up from here.
  bg1: "#333a48",
  bg2: "#3d4556",
  bg3: "#4a5365",
  glass: "#333a48",

  line: "#6b7688",
  edge: "#8794a8",

  // The lamp. `cyan` is the primary-interactive slot rather than a hue
  // promise, so here it is the orange everything active is lit with.
  cyan: "#f0883c",
  cyanBright: "#ffa666",
  cyanDeep: "#8a4a18",
  // Deliberately desaturated next to the accent: these exist to mark states,
  // not to compete for attention with it.
  magenta: "#e0698a",
  amber: "#f0b429",
  amberSunken: "#2a2113",
  red: "#e05252",
  redSoft: "#ef9494",
  violet: "#8b7fd4",
  green: "#4fbf87",
  orange: "#f0883c",

  text: "#d5dbe5",
  textMid: "#9aa5b6",
  // Not the #74808f this palette wants by eye: that lands at 2.84 on the
  // module face and the readability test holds secondary text to 3.0.
  textDim: "#828f9f",
  textFaint: "#69747f",
  // Dark ink on the orange lamp — white on it is unreadable.
  textOnAccent: "#1a1206",

  // Cool and near-black rather than pure black, so shadows on slate stay in
  // the same family as the surface casting them.
  shadow: "#10131a",

  trackPalette: ["#f0883c", "#e0698a", "#f0b429", "#8b7fd4", "#4fbf87", "#5aa9d6"],

  borderWidth: "1px",
  focusWidth: "2px",
  // Solid all the way through: a module is an object, not a window.
  glassBlur: "0px",
  glassAlpha: "1",
  panelAlpha: "1",
  // A trace, for lit values and meters only — the panel itself never glows.
  glowScale: "0.15",
  bodyGlow: "0",

  // material — the face gradient does the heavy lifting, so `sheen` is the
  // highest of any built-in while `grain` stays low: this is painted metal
  // with a smooth finish, not bead-blasted aluminium.
  bevel: "0.7",
  relief: "0.75",
  sheen: "0.85",
  grain: "0.12",
  ctrlRadius: "4px",
};
