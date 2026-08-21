/**
 * The built-in theme registry. Order here is the order the preferences
 * picker lists them in, so it is curated, not alphabetical: the house theme
 * first, then the two accessibility themes, then the borrowed palettes.
 */

import type { ThemeTokens } from "../tokens";
import { AURA_DARK_TOKENS } from "./aura-dark";
import { AURA_LIGHT_TOKENS } from "./aura-light";
import { CONSOLE_NOIR_TOKENS } from "./console-noir";
import { STUDIO_IVORY_TOKENS } from "./studio-ivory";
import { RACK_SLATE_TOKENS } from "./rack-slate";
import { HIGH_CONTRAST_DARK_TOKENS } from "./high-contrast-dark";
import { HIGH_CONTRAST_LIGHT_TOKENS } from "./high-contrast-light";
import { SOLARIZED_DARK_TOKENS } from "./solarized-dark";
import { SOLARIZED_LIGHT_TOKENS } from "./solarized-light";
import { NORD_TOKENS } from "./nord";
import { GRUVBOX_DARK_TOKENS } from "./gruvbox-dark";

export interface Theme {
  /** Stable identifier; the value stored in the `theme` preference. */
  id: string;
  /** Human name shown in the picker. */
  name: string;
  /** The built-in this theme resolves against. A built-in is its own base. */
  base: string;
  /** Fully resolved — never partial at the point of use. */
  tokens: ThemeTokens;
  source: "builtin" | "user";
}

function builtin(id: string, name: string, tokens: ThemeTokens): Theme {
  return { id, name, base: id, tokens, source: "builtin" };
}

export const AURA_DARK = builtin("aura-dark", "AURA Dark", AURA_DARK_TOKENS);
export const AURA_LIGHT = builtin("aura-light", "AURA Light", AURA_LIGHT_TOKENS);
export const CONSOLE_NOIR = builtin("console-noir", "Console Noir", CONSOLE_NOIR_TOKENS);
export const STUDIO_IVORY = builtin("studio-ivory", "Studio Ivory", STUDIO_IVORY_TOKENS);
export const RACK_SLATE = builtin("rack-slate", "Rack Slate", RACK_SLATE_TOKENS);
export const HIGH_CONTRAST_DARK = builtin(
  "high-contrast-dark",
  "High Contrast Dark",
  HIGH_CONTRAST_DARK_TOKENS,
);
export const HIGH_CONTRAST_LIGHT = builtin(
  "high-contrast-light",
  "High Contrast Light",
  HIGH_CONTRAST_LIGHT_TOKENS,
);
export const SOLARIZED_DARK = builtin("solarized-dark", "Solarized Dark", SOLARIZED_DARK_TOKENS);
export const SOLARIZED_LIGHT = builtin("solarized-light", "Solarized Light", SOLARIZED_LIGHT_TOKENS);
export const NORD = builtin("nord", "Nord", NORD_TOKENS);
export const GRUVBOX_DARK = builtin("gruvbox-dark", "Gruvbox Dark", GRUVBOX_DARK_TOKENS);

// Picker order: the house themes, then the two material themes — they are
// the showcase for the material tokens and the first thing worth trying —
// then the two accessibility themes, which are why this exists and should
// not be buried, and finally the borrowed palettes.
export const BUILTIN_THEMES: readonly Theme[] = [
  AURA_DARK,
  AURA_LIGHT,
  RACK_SLATE,
  CONSOLE_NOIR,
  STUDIO_IVORY,
  HIGH_CONTRAST_DARK,
  HIGH_CONTRAST_LIGHT,
  SOLARIZED_DARK,
  SOLARIZED_LIGHT,
  NORD,
  GRUVBOX_DARK,
];

// Null prototype on purpose: theme ids come from untrusted filenames and
// preference blobs, and on a normal object `BUILTIN_BY_ID["__proto__"]` and
// `["constructor"]` are truthy — a lookup that would pass validation and then
// hand back something that is not a theme.
export const BUILTIN_BY_ID: Readonly<Record<string, Theme>> = Object.assign(
  Object.create(null) as Record<string, Theme>,
  Object.fromEntries(BUILTIN_THEMES.map((t) => [t.id, t])),
);

export type BuiltinId =
  | "aura-dark"
  | "aura-light"
  | "console-noir"
  | "studio-ivory"
  | "rack-slate"
  | "high-contrast-dark"
  | "high-contrast-light"
  | "solarized-dark"
  | "solarized-light"
  | "nord"
  | "gruvbox-dark";

export const DEFAULT_THEME_ID = AURA_DARK.id;
