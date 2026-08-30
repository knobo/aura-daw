/**
 * Brushed Steel Light — the same panel in bare stainless rather than dark
 * anodised, photographed in a bright room.
 *
 * Its point is that it is NOT the dark theme with the colours flipped. Bare
 * steel is lighter than the ground it stands on, so the surface ramp runs
 * upward from the panel exactly as it does under Studio Ivory, and every
 * `bg2`/`bg3` call site keeps working because those ask for "more raised",
 * not "brighter". What changes with the light is the LIGHTING, and that is
 * two values: `relief` drops (a bright room has softer shadows) and
 * `glowScale` all but goes out — a lamp that reads as a glow on a dark panel
 * reads as a smudge on a bright one, and its colour has to carry it instead.
 *
 * `shadow` is a cool blue-grey, not black. It is the light-theme counterpart
 * to Studio Ivory's warm brown one and for the same reason: a real shadow on
 * a surface carries that surface's own hue, and a black shadow on steel
 * makes the panel look like a page with holes cut in it.
 *
 * The accents are the dark half of the same hues the dark theme uses, chosen
 * so text and track colours clear AA against the panel rather than for their
 * relationship to each other.
 */

import type { ThemeTokens } from "../tokens";

export const BRUSHED_STEEL_LIGHT_TOKENS: ThemeTokens = {
  bgVoid: "#8f979f",
  bg0: "#b2b9c1",
  bgSunken: "#bec5cd",
  bg1: "#cdd3da",
  // Up the ramp is LIGHTER here, as on every light theme.
  bg2: "#d9dfe5",
  bg3: "#e5eaef",
  glass: "#cdd3da",

  line: "#6f7883",
  edge: "#58616c",

  cyan: "#3d7d10",
  cyanBright: "#56a318",
  cyanDeep: "#21460a",
  magenta: "#a83070",
  amber: "#8a6207",
  amberSunken: "#ece0c4",
  red: "#b8352c",
  redSoft: "#d98a83",
  violet: "#5d4bb0",
  green: "#3d7d10",
  orange: "#a35410",

  text: "#14181c",
  textMid: "#414952",
  textDim: "#5c6570",
  textFaint: "#7d8792",
  textOnAccent: "#ffffff",

  // Cool steel-grey — see the note above.
  shadow: "#454e57",

  trackPalette: ["#3d7d10", "#1c6a94", "#a83070", "#8a6207", "#5d4bb0", "#a35410"],

  borderWidth: "1px",
  focusWidth: "2px",
  glassBlur: "0px",
  glassAlpha: "1",
  panelAlpha: "1",
  // Almost out: in a bright room the lamp's colour is what reads, not its
  // bloom. Not zero, because the knob's lit dots still want a soft edge.
  glowScale: "0.2",
  bodyGlow: "0",

  // material — the same finish under a softer light: shallower cast shadow,
  // and a satin rather than specular sheen. `sheen` is held BELOW the dark
  // half rather than level with it, which is the one place the light theme
  // cannot simply mirror: the dome highlight is white, and on a surface that
  // is already near-white it stops reading as a highlight and starts reading
  // as blown-out plastic. The panel ramp sits a step lower for the same
  // reason — the knob cap is the top of that ramp.
  bevel: "0.8",
  relief: "0.6",
  sheen: "0.45",
  grain: "0.12",
  // Lower than the dark half: `overlay` blending brightens more than it
  // darkens on a light backdrop, so the same number reads as a busier
  // surface here than it does on anodised grey.
  brush: "0.6",
  ctrlRadius: "3px",
};
