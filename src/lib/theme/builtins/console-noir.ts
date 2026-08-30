/**
 * Console Noir — the flagship material theme: a machined outboard rack unit.
 *
 * Where AURA Dark is a screen (blue-black void, emissive cyan, glass), this
 * is an OBJECT: bead-blasted graphite aluminium, warm silkscreen legends, and
 * an amber lamp as the primary interactive colour rather than a neon glow.
 * The palette is deliberately warm and near-neutral — real anodised panels
 * carry almost no hue, and it is the bevel, the grain and the cast shadow
 * that do the work here, not saturation.
 *
 * The glass and glow affordances are turned nearly off on purpose. A frosted
 * translucent panel and a milled metal one are two different claims about
 * what the surface IS, and a theme that makes both at once looks like
 * neither. `glowScale` keeps a residue rather than going to zero: a lit LED
 * on real hardware does bloom slightly, and the transport lamps read as dead
 * plastic without it.
 */

import type { ThemeTokens } from "../tokens";

export const CONSOLE_NOIR_TOKENS: ThemeTokens = {
  bgVoid: "#0a0a0b",
  bg0: "#131315",
  bgSunken: "#0d0d0f",
  bg1: "#1a1a1d",
  bg2: "#232327",
  bg3: "#2e2e33",
  glass: "#1a1a1d",

  line: "#6b6b73",
  edge: "#8a8a95",

  // The primary interactive colour is a lamp, not a laser: warm amber-gold,
  // the colour of every VU backlight and every "on" legend on a rack unit.
  cyan: "#e8a33d",
  cyanBright: "#ffc978",
  cyanDeep: "#7a5215",
  magenta: "#d8506b",
  amber: "#f2c14e",
  amberSunken: "#1d1608",
  red: "#e2483f",
  redSoft: "#f0968f",
  violet: "#9b8ec4",
  green: "#6fbf8b",
  orange: "#e0783c",

  // Warm off-white, the colour screen-printed legends actually are — a pure
  // #ffffff label on a graphite panel reads as a sticker.
  text: "#e6e2da",
  textMid: "#a8a196",
  textDim: "#77706a",
  textFaint: "#4d4842",
  // Dark ink on the amber lamp; white on amber is unreadable.
  textOnAccent: "#14100a",

  shadow: "#000000",

  trackPalette: ["#e8a33d", "#d8506b", "#6fbf8b", "#5f9ea0", "#b48ead", "#e0783c"],

  borderWidth: "1px",
  focusWidth: "2px",
  // Solid panels: metal is not frosted glass.
  glassBlur: "0px",
  glassAlpha: "1",
  // Not zero — see the note above about LEDs.
  glowScale: "0.25",
  bodyGlow: "0.02",
  // Milled metal is not a window.
  panelAlpha: "1",

  // material — the point of the theme. Everything near the top of its range:
  // hard machined edges, a deep cast shadow, a strong sheen down each face,
  // heavy bead-blast grain, and the tight 3px corner of a milled panel.
  bevel: "0.95",
  relief: "0.9",
  sheen: "0.7",
  grain: "0.38",
  // Not brushed: this surface has no direction to it.
  brush: "0",
  ctrlRadius: "3px",
};
