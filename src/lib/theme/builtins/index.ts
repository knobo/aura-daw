/**
 * The built-in theme registry. Order here is the order the preferences
 * picker lists them in, so it is curated, not alphabetical: the house theme
 * first, then the two accessibility themes, then the borrowed palettes.
 */

import type { ThemeTokens } from "../tokens";
import { AURA_DARK_TOKENS } from "./aura-dark";

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

export const BUILTIN_THEMES: readonly Theme[] = [AURA_DARK];

export const BUILTIN_BY_ID: Readonly<Record<string, Theme>> = Object.fromEntries(
  BUILTIN_THEMES.map((t) => [t.id, t]),
);

export type BuiltinId = string;

export const DEFAULT_THEME_ID = AURA_DARK.id;
