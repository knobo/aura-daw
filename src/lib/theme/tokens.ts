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
  /** Glow radius. `0px` makes every 0-offset box-shadow render nothing. */
  glowBlur: string;
  /** Alpha of the two radial washes on `body`; `0` flattens the background. */
  bodyGlow: string;
}

export const COLOR_KEYS = [
  "bgVoid", "bg0", "bgSunken", "bg1", "bg2", "bg3", "glass",
  "line", "edge",
  "cyan", "cyanBright", "cyanDeep", "magenta", "amber", "amberSunken",
  "red", "redSoft", "violet", "green", "orange",
  "text", "textMid", "textDim", "textFaint", "textOnAccent",
  "shadow",
] as const satisfies readonly (keyof ThemeTokens)[];

export const AFFORDANCE_KEYS = [
  "borderWidth", "focusWidth", "glassBlur", "glowBlur", "bodyGlow",
] as const satisfies readonly (keyof ThemeTokens)[];

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
  vars["--glass"] = alpha(t.glass, 0.62);
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
