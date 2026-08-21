/**
 * The theme token contract: the complete set of colours and affordances a
 * theme owns, and the translation from that TypeScript shape into CSS custom
 * properties.
 *
 * Every colour is emitted TWICE — `--cyan: #52e5ff` and
 * `--cyan-rgb: 82 229 255` — so components can write an alpha variant as
 * `rgb(var(--cyan-rgb) / 0.4)`. That is the portable spelling: CSS relative
 * colour syntax (`rgb(from var(--cyan) r g b / 0.4)`) is newer than the
 * WebKitGTK baseline AURA assumes on Linux.
 *
 * Pure TypeScript on purpose (no runes, no DOM): importable from node tests,
 * and `applyTokens` takes a structural slice of its target rather than an
 * HTMLElement — the same trick `utils/ui-zoom.ts` uses for the same reason.
 */

export interface ThemeTokens {
  /* surfaces, deepest → most raised */
  bgVoid: string;
  bg0: string;
  bgSunken: string;
  bg1: string;
  bg2: string;
  bg3: string;
  /** Base colour the translucent panel fill is mixed from. */
  glass: string;

  /* hairlines: `line` for grids and rulers, `edge` for panel borders */
  line: string;
  edge: string;

  /* accents */
  cyan: string;
  cyanBright: string;
  cyanDeep: string;
  magenta: string;
  amber: string;
  amberSunken: string;
  red: string;
  redSoft: string;
  violet: string;
  green: string;
  orange: string;

  /* text ramp */
  text: string;
  textMid: string;
  textDim: string;
  textFaint: string;
  /** Text drawn ON a saturated accent (record buttons, badges). */
  textOnAccent: string;

  /** Base for drop shadows and inner glows. */
  shadow: string;

  /** Clip and track identity ramp. Exactly six; a light theme needs its own. */
  trackPalette: readonly [string, string, string, string, string, string];

  /* affordances — the tokens a high-contrast theme leans on */
  /** Panel and control border thickness. */
  borderWidth: string;
  /** Focus-ring thickness. */
  focusWidth: string;
  /** Backdrop blur on glass panels; `0px` removes the frosted look. */
  glassBlur: string;
  /** Opacity of the glass panel fill, 0..1. `1` makes panels solid — which is
   * what a theme wants whenever `glassBlur` is `0px`, since with no blur to
   * soften it a translucent panel shows the raw grid through its own text. */
  glassAlpha: string;
  /** Multiplier on every glow radius, unitless. `1` is the designed radius;
   * `0` collapses every glow to nothing. A scale rather than a length so each
   * call site keeps its own designed radius — and so radius ANIMATIONS keep
   * their two ends apart. */
  glowScale: string;
  /** Alpha of the two radial washes on `body`; `0` flattens the background. */
  bodyGlow: string;

  /* ── materials: what a surface is MADE OF, as opposed to what colour it is ──
   *
   * The palette above answers "what colour is this panel". These five answer
   * "is it milled aluminium, moulded plastic, or flat vector" — and they are
   * what let one palette ship as both a flat theme and a hardware theme. The
   * virtual key light is fixed at top-centre, the convention every real
   * front panel is photographed under, so a raised face is lighter along its
   * top edge and casts downward. All four scalars are 0..1 and all four at
   * `0` collapse to exactly the flat look AURA had before they existed —
   * which is what the high-contrast themes rely on. */
  /** Strength of the lit top edge and shadowed bottom edge on a raised face
   * (and inverted, the lip of a recessed well). This is the token that reads
   * as "moulded" — it is the edge, not the shadow under it. */
  bevel: string;
  /** Depth multiplier on the shadow a raised element CASTS on the panel
   * behind it. Separate from `bevel` because a thick-edged button lying flat
   * on the panel and a thin card floating above it are different objects. */
  relief: string;
  /** Strength of the specular gradient down a face — the sweep of reflected
   * light that makes a knob cap read as domed rather than as a circle. */
  sheen: string;
  /** Opacity of the micro-texture overlay: the fine speckle of bead-blasted
   * metal or moulded plastic. Cheap to ignore, and the single cue that most
   * separates "photograph of hardware" from "rectangle with a gradient". */
  grain: string;
  /** Corner radius of controls — buttons, chips, wells. A length, because it
   * is a physical dimension rather than a strength: hard-edged rack gear and
   * soft-cornered consumer plastic differ here and nowhere else. */
  ctrlRadius: string;
}

export const COLOR_KEYS = [
  "bgVoid", "bg0", "bgSunken", "bg1", "bg2", "bg3", "glass",
  "line", "edge",
  "cyan", "cyanBright", "cyanDeep", "magenta", "amber", "amberSunken",
  "red", "redSoft", "violet", "green", "orange",
  "text", "textMid", "textDim", "textFaint", "textOnAccent",
  "shadow",
] as const satisfies readonly (keyof ThemeTokens)[];

/** The material group, named separately because it is the axis a theme
 * author tunes as a set: all five together are one material, and mixing
 * half of one with half of another is how a panel stops looking real. */
export const MATERIAL_KEYS = [
  "bevel", "relief", "sheen", "grain", "ctrlRadius",
] as const satisfies readonly (keyof ThemeTokens)[];

export const AFFORDANCE_KEYS = [
  "borderWidth", "focusWidth", "glassBlur", "glassAlpha", "glowScale", "bodyGlow",
  ...MATERIAL_KEYS,
] as const satisfies readonly (keyof ThemeTokens)[];

/** The affordances measured as a bare number rather than a CSS length. */
export const UNITLESS_AFFORDANCE_KEYS: readonly (keyof ThemeTokens)[] = [
  "glassAlpha", "glowScale", "bodyGlow",
  // The four material strengths are ratios; `ctrlRadius` alone is a length.
  "bevel", "relief", "sheen", "grain",
];

/** Every key a theme must define — the runtime half of the interface. */
export const TOKEN_KEYS: readonly (keyof ThemeTokens)[] = [
  ...COLOR_KEYS,
  "trackPalette",
  ...AFFORDANCE_KEYS,
];

/** Structural slice of an element's style, so this module needs no DOM. */
export type StyleTarget = { style: { setProperty(name: string, value: string): void } };

/** r, g, b, a (a in 0..1) parsed from any colour spelling a theme may use. */
function channels(color: string): [number, number, number, number] {
  const s = color.trim();
  if (s.startsWith("#")) {
    const h = s.slice(1);
    const wide = h.length <= 4 ? h.replace(/./g, (c) => c + c) : h;
    const n = (i: number) => parseInt(wide.slice(i, i + 2), 16);
    return [n(0), n(2), n(4), wide.length >= 8 ? n(6) / 255 : 1];
  }
  const parts = s.replace(/^rgba?\(|\)$/g, "").split(/[,\s/]+/).filter(Boolean);
  return [Number(parts[0]), Number(parts[1]), Number(parts[2]), parts[3] === undefined ? 1 : Number(parts[3])];
}

/** `"82 229 255"` — the space-separated form `rgb(var(--x-rgb) / a)` needs. */
export function rgbTriple(color: string): string {
  const [r, g, b] = channels(color);
  return `${r} ${g} ${b}`;
}

/** `rgb(r g b / a)`, multiplying through any alpha the input already carries. */
export function alpha(color: string, a: number): string {
  const [r, g, b, own] = channels(color);
  const out = Math.min(1, Math.max(0, a)) * own;
  return `rgb(${r} ${g} ${b} / ${Math.round(out * 1000) / 1000})`;
}

/** A unitless token as a number, falling back rather than emitting `NaN`. */
function number(value: string, fallback: number): number {
  const n = Number(value);
  return Number.isFinite(n) ? n : fallback;
}

/** camelCase token key → kebab-case CSS custom property name. */
function cssName(key: string): string {
  return "--" + key.replace(/([a-z])([A-Z0-9])/g, "$1-$2").toLowerCase();
}

/**
 * The full custom-property map for a theme. Derived tokens (`--glass`,
 * `--grid-line`, …) are emitted as LITERAL colours rather than as
 * `var()` references so the map has no internal ordering dependencies and
 * a single `setProperty` sweep is enough.
 */
export function toCssVars(t: ThemeTokens): Record<string, string> {
  const vars: Record<string, string> = {};

  for (const key of COLOR_KEYS) {
    const value = t[key] as string;
    vars[cssName(key)] = value;
    vars[cssName(key) + "-rgb"] = rgbTriple(value);
  }

  t.trackPalette.forEach((c, i) => {
    vars[`--track-${i + 1}`] = c;
    vars[`--track-${i + 1}-rgb`] = rgbTriple(c);
  });

  for (const key of AFFORDANCE_KEYS) vars[cssName(key)] = t[key] as string;

  // Derived tokens app.css has always exposed, at their established alphas,
  // so every existing var() call site keeps working untouched.
  vars["--glass-base-rgb"] = rgbTriple(t.glass);
  vars["--glass"] = alpha(t.glass, number(t.glassAlpha, 1));
  vars["--glass-border"] = alpha(t.edge, 0.12);
  vars["--grid-line"] = alpha(t.line, 0.09);
  vars["--grid-line-strong"] = alpha(t.line, 0.2);
  vars["--cyan-dim"] = alpha(t.cyan, 0.35);
  vars["--magenta-dim"] = alpha(t.magenta, 0.35);

  return vars;
}

/** Write a resolved theme onto an element's inline style. */
export function applyTokens(el: StyleTarget, t: ThemeTokens) {
  for (const [name, value] of Object.entries(toCssVars(t))) el.style.setProperty(name, value);
}
