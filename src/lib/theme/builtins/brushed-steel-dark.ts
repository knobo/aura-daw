/**
 * Brushed Steel Dark — a dark-anodised steel front panel under one green lamp.
 *
 * The third material theme, and the one that exists to give the `brush`
 * token something to be. Console Noir is bead-blasted (speckle, no
 * direction); Studio Ivory is moulded plastic (neither); this is metal that
 * went past a wire wheel, so it carries long fine horizontal streaks and
 * only a trace of speckle over them. That directionality is the entire
 * difference between "dark grey panel" and "steel", and it costs one token.
 *
 * Three decisions carry it:
 *
 * - **The streaks run, the speckle does not.** `brush` at 0.85 with `grain`
 *   held down at 0.14. Raise the grain to match and the finish turns back
 *   into cast metal; the two are not interchangeable strengths.
 * - **A hard 3px corner.** Steel is cut, not moulded. `ctrlRadius` is where
 *   that shows, and a 9px corner on a brushed face reads as painted plastic
 *   no matter what the texture says.
 * - **One green lamp.** The primary-interactive slot points at the LED green
 *   the knob's dot ring is lit with, the way Rack Slate points it at orange.
 *   Everything not lit is grey — a second saturated accent competing with the
 *   lamp is what turns a front panel back into a website.
 *
 * `shadow` is a cool near-black rather than pure black: shadows on steel
 * carry the surface's own blue-grey, and a pure black one reads as a hole
 * punched through the panel rather than as the panel's own thickness.
 */

import type { ThemeTokens } from "../tokens";

export const BRUSHED_STEEL_DARK_TOKENS: ThemeTokens = {
  // The gutter the panels are bolted into.
  bgVoid: "#15181b",
  bg0: "#1c2024",
  bgSunken: "#23282d",
  // The panel face. Everything raised steps up from here.
  bg1: "#2b3035",
  bg2: "#363c43",
  bg3: "#444b54",
  glass: "#2b3035",

  line: "#7f8a97",
  edge: "#9dabba",

  // The lamp. `cyan` is the primary-interactive slot rather than a hue
  // promise, so here it is the LED green everything active is lit with.
  cyan: "#8ede2a",
  cyanBright: "#b4f05e",
  cyanDeep: "#3f6a12",
  // Deliberately held back next to the lamp: these mark states, they do not
  // compete with it for attention.
  magenta: "#e57ab0",
  amber: "#e8b53c",
  amberSunken: "#2a2210",
  red: "#ec6a62",
  redSoft: "#f5a09a",
  violet: "#9f8fe0",
  green: "#8ede2a",
  orange: "#ef9350",

  text: "#e2e8ee",
  textMid: "#a7b2be",
  textDim: "#8892a0",
  textFaint: "#69737e",
  // Dark ink on the green lamp — white on it is unreadable.
  textOnAccent: "#10200a",

  // Cool, not black — see the note above.
  shadow: "#0b0e11",

  trackPalette: ["#8ede2a", "#63b6e8", "#e57ab0", "#e8b53c", "#9f8fe0", "#ef9350"],

  borderWidth: "1px",
  focusWidth: "2px",
  // Solid all the way through: a steel panel is an object, not a window.
  glassBlur: "0px",
  glassAlpha: "1",
  panelAlpha: "1",
  // The lamps glow; the metal does not.
  glowScale: "0.55",
  bodyGlow: "0",

  // material — brushed, barely speckled, deeply moulded at the edges, and
  // cut square. The `brush`/`grain` split is the theme's whole claim.
  bevel: "0.9",
  relief: "0.8",
  sheen: "0.6",
  grain: "0.14",
  brush: "0.85",
  ctrlRadius: "3px",
};
