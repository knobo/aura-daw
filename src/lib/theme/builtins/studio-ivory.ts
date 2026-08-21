/**
 * Studio Ivory — the light half of the material pair: cream moulded plastic.
 *
 * The same material machinery as Console Noir pointed at a different object.
 * Where that one is milled metal, this is an injection-moulded desktop box:
 * softer corners, a shallower cast shadow, a satin rather than specular
 * sheen. It exists partly as a light theme worth using and partly as the
 * proof that the material tokens are genuinely orthogonal to the palette —
 * nothing here is a dark theme with the colours flipped.
 *
 * `shadow` is a warm brown-grey rather than black, and that single value is
 * most of why this reads as a photographed object. Black shadows on a cream
 * panel look like holes cut in it; real shadows on a warm surface carry the
 * surface's own hue.
 */

import type { ThemeTokens } from "../tokens";

export const STUDIO_IVORY_TOKENS: ThemeTokens = {
  bgVoid: "#b9b3a8",
  bg0: "#ded8cc",
  bgSunken: "#cfc8bb",
  bg1: "#eae4d8",
  // On a light theme the raised surfaces are LIGHTER than the panel, not
  // darker: the ramp runs the other way and every `bg2`/`bg3` call site
  // keeps working because it asks for "more raised", not "brighter".
  bg2: "#f3eee4",
  bg3: "#fbf8f1",
  glass: "#eae4d8",

  line: "#8d8577",
  edge: "#6f6759",

  cyan: "#1d7f8c",
  cyanBright: "#2ba0b0",
  cyanDeep: "#0e4a52",
  magenta: "#b5397a",
  amber: "#9c6608",
  amberSunken: "#f0e2c4",
  red: "#c0392b",
  redSoft: "#e08c82",
  violet: "#6d5bb5",
  green: "#2e8b57",
  orange: "#b0500f",

  text: "#241f18",
  textMid: "#4f4739",
  textDim: "#756c5c",
  textFaint: "#9a9182",
  textOnAccent: "#ffffff",

  // Warm, not black — see the note above.
  shadow: "#3a3227",

  trackPalette: ["#1d7f8c", "#b5397a", "#9c6608", "#6d5bb5", "#2e8b57", "#b0500f"],

  borderWidth: "1px",
  focusWidth: "2px",
  glassBlur: "0px",
  glassAlpha: "1",
  // Nothing on a plastic box emits light.
  glowScale: "0",
  bodyGlow: "0",
  // Moulded plastic is not a window either.
  panelAlpha: "1",

  // material — moulded rather than milled: a softer bevel, a shallower cast
  // shadow, satin sheen, fine plastic texture, and a generous 9px corner.
  bevel: "0.85",
  relief: "0.65",
  sheen: "0.55",
  grain: "0.28",
  ctrlRadius: "9px",
};
