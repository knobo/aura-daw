/**
 * Untrusted theme JSON → a resolved Theme. Never throws.
 *
 * Two levels of failure, deliberately different: a file whose IDENTITY is
 * broken (unparseable, nameless, extending nothing real) is rejected whole,
 * because there is nothing to show in the picker. A file whose identity is
 * fine but which carries a bad VALUE keeps loading with that key dropped —
 * one typo should not cost the user their theme. Both paths report, so the
 * caller can say what happened instead of failing silently.
 *
 * A user theme may only extend a BUILT-IN. That is what removes cycle
 * resolution across untrusted files entirely.
 */

import {
  AFFORDANCE_KEYS,
  COLOR_KEYS,
  UNITLESS_AFFORDANCE_KEYS,
  type ThemeTokens,
} from "./tokens";
import { BUILTIN_BY_ID, DEFAULT_THEME_ID, type Theme } from "./builtins/index";

export type ParsedTheme =
  | { ok: true; theme: Theme; dropped: string[] }
  | { ok: false; reason: string };

// Only the spellings `channels()` in tokens.ts can actually read. A looser
// pattern (`rgba?\([^)]*\)`) would accept `rgb(80% 50% 20%)`, report nothing
// dropped, and then emit `--cyan-rgb: NaN NaN NaN` — a theme that loads
// "successfully" and paints nothing.
const NUM = String.raw`[0-9]*\.?[0-9]+`;
const COLOR_RE = new RegExp(
  String.raw`^(#([0-9a-f]{3,4}|[0-9a-f]{6}|[0-9a-f]{8})` +
    String.raw`|rgba?\(\s*${NUM}\s*[,\s]\s*${NUM}\s*[,\s]\s*${NUM}\s*([,/]\s*${NUM}\s*)?\))$`,
  "i",
);
const LENGTH_RE = /^(0|[0-9]*\.?[0-9]+(px|rem|em))$/;
const UNITLESS_RE = /^[0-9]*\.?[0-9]+$/;

const COLOR_SET = new Set<string>(COLOR_KEYS);
const AFFORDANCE_SET = new Set<string>(AFFORDANCE_KEYS);
const UNITLESS_SET = new Set<string>(UNITLESS_AFFORDANCE_KEYS);

function validAffordance(key: string, value: string): boolean {
  return UNITLESS_SET.has(key) ? UNITLESS_RE.test(value) : LENGTH_RE.test(value);
}

/** A built-in by id — `hasOwn`, so `__proto__` and `constructor` are not
 * theme names. Without it a file extending `"__proto__"` validates and then
 * registers a theme with no tokens at all. */
function builtin(id: string): Theme | undefined {
  return Object.hasOwn(BUILTIN_BY_ID, id) ? BUILTIN_BY_ID[id] : undefined;
}

export function parseUserTheme(id: string, raw: string): ParsedTheme {
  if (builtin(id)) return { ok: false, reason: `"${id}" is the id of a built-in theme` };

  let doc: unknown;
  try {
    doc = JSON.parse(raw);
  } catch {
    return { ok: false, reason: "not valid JSON" };
  }
  if (!doc || typeof doc !== "object" || Array.isArray(doc)) {
    return { ok: false, reason: "the top level must be a JSON object" };
  }

  const { name, extends: ext, tokens } = doc as Record<string, unknown>;
  if (typeof name !== "string" || name.trim() === "") {
    return { ok: false, reason: '"name" is required and must not be blank' };
  }

  const baseId = ext === undefined ? DEFAULT_THEME_ID : ext;
  const base = typeof baseId === "string" ? builtin(baseId) : undefined;
  if (!base) {
    return { ok: false, reason: `"extends": ${JSON.stringify(ext)} is not a built-in theme` };
  }

  if (tokens !== undefined && (typeof tokens !== "object" || tokens === null || Array.isArray(tokens))) {
    return { ok: false, reason: '"tokens" must be a JSON object' };
  }

  const merged: ThemeTokens = { ...base.tokens };
  const dropped: string[] = [];

  for (const [key, value] of Object.entries((tokens ?? {}) as Record<string, unknown>)) {
    if (key === "trackPalette") {
      const ok =
        Array.isArray(value) &&
        value.length === 6 &&
        value.every((c) => typeof c === "string" && COLOR_RE.test(c));
      if (ok) merged.trackPalette = value as unknown as ThemeTokens["trackPalette"];
      else dropped.push(key);
      continue;
    }
    if (typeof value !== "string") {
      dropped.push(key);
      continue;
    }
    if (COLOR_SET.has(key)) {
      if (COLOR_RE.test(value.trim())) (merged as unknown as Record<string, unknown>)[key] = value.trim();
      else dropped.push(key);
      continue;
    }
    if (AFFORDANCE_SET.has(key)) {
      if (validAffordance(key, value.trim())) (merged as unknown as Record<string, unknown>)[key] = value.trim();
      else dropped.push(key);
      continue;
    }
    dropped.push(key);
  }

  return {
    ok: true,
    theme: { id, name: name.trim(), base: base.id, tokens: merged, source: "user" },
    dropped,
  };
}
