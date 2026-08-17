/**
 * The live theme: which theme is active, its fully-resolved tokens, and the
 * write-through to CSS custom properties on the document root.
 *
 * `tokens` is `$state` and is REPLACED wholesale on every apply, never
 * mutated in place. That is what makes canvas drawing work: the `$effect`s
 * in Timeline, PianoRoll, Meter and friends read `theme.tokens.<key>`
 * directly, so a theme change re-runs them exactly once and they repaint —
 * no getComputedStyle, nothing to invalidate by hand.
 *
 * `wantedId` is separate from `activeId` on purpose. User themes arrive
 * asynchronously from the backend; a preference naming one is remembered
 * as a wish and honoured the moment the file shows up.
 */

import { applyTokens, TOKEN_KEYS, type StyleTarget, type ThemeTokens } from "./tokens";
import { BUILTIN_BY_ID, BUILTIN_THEMES, DEFAULT_THEME_ID, type Theme } from "./builtins/index";

/** Last applied theme, so a user theme paints correctly on the next boot
 * before the backend has listed the themes folder. */
export const THEME_CACHE_KEY = "aura.theme.cache";

function root(): StyleTarget | undefined {
  return typeof document === "undefined" ? undefined : document.documentElement;
}

/**
 * A cached entry, or nothing. The token check is exhaustive on purpose: the
 * cache outlives the build that wrote it, and `applyTokens` would throw on a
 * missing key — from inside the mount effect, taking the whole boot with it.
 * A cache that no longer matches the token contract is simply not a cache.
 */
function readCache(): { id: string; tokens: ThemeTokens } | undefined {
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem(THEME_CACHE_KEY) ?? "");
    if (!parsed || typeof parsed !== "object") return undefined;
    const { id, tokens } = parsed as { id?: unknown; tokens?: unknown };
    if (typeof id !== "string" || id === "") return undefined;
    if (!tokens || typeof tokens !== "object") return undefined;
    const t = tokens as Record<string, unknown>;
    for (const key of TOKEN_KEYS) {
      const value = t[key];
      if (key === "trackPalette") {
        if (!Array.isArray(value) || value.length !== 6) return undefined;
        if (!value.every((c) => typeof c === "string")) return undefined;
      } else if (typeof value !== "string") {
        return undefined;
      }
    }
    return { id, tokens: tokens as ThemeTokens };
  } catch {
    // corrupt, missing, or no storage — bootstrap falls through to default
  }
  return undefined;
}

function writeCache(id: string, tokens: ThemeTokens) {
  try {
    localStorage.setItem(THEME_CACHE_KEY, JSON.stringify({ id, tokens }));
  } catch {
    // quota, private mode, node tests — the cache is disposable
  }
}

class ThemeStore {
  /** The resolved token set. Replaced, never mutated. */
  tokens = $state<ThemeTokens>(BUILTIN_BY_ID[DEFAULT_THEME_ID].tokens);
  /** The theme actually painted right now. */
  activeId = $state<string>(DEFAULT_THEME_ID);

  /** The theme the preference asks for — may not exist yet (async user files). */
  private wantedId = DEFAULT_THEME_ID;
  private user = $state<Theme[]>([]);

  builtins(): Theme[] {
    return [...BUILTIN_THEMES];
  }

  userThemes(): Theme[] {
    return [...this.user];
  }

  /** The theme for an id, or the default when the id is unknown. */
  resolve(id: string): Theme {
    if (Object.hasOwn(BUILTIN_BY_ID, id)) return BUILTIN_BY_ID[id];
    return this.user.find((t) => t.id === id) ?? BUILTIN_BY_ID[DEFAULT_THEME_ID];
  }

  /** Paint a resolved theme without touching the wish or the boot cache. */
  private paint(t: Theme, target: StyleTarget | undefined) {
    this.activeId = t.id;
    this.tokens = t.tokens;
    if (target) applyTokens(target, t.tokens);
  }

  /**
   * Record the wish and paint whatever it resolves to. Only a wish that
   * actually resolved is cached — caching the fallback would replace a user
   * theme's tokens with the default's and permanently disable `bootstrap`.
   *
   * `settled` says whether the backend has answered yet. Until it has, a
   * wish that does not resolve leaves an existing paint FOR THAT SAME ID
   * alone: the preference effect runs right after `bootstrap`, long before
   * `loadUserThemes` returns, and repainting there is exactly the
   * light-to-dark flash the boot cache exists to prevent.
   */
  private set(id: string, target: StyleTarget | undefined, settled: boolean) {
    this.wantedId = id;
    const resolved = this.resolve(id);
    if (!settled && resolved.id !== id && this.activeId === id) return;
    this.paint(resolved, target);
    if (resolved.id === id) writeCache(resolved.id, resolved.tokens);
  }

  /**
   * Paint a theme. An unknown id degrades to the default rather than
   * leaving the app unstyled — the same contract `coercePref` follows.
   */
  apply(id: string, target: StyleTarget | undefined = root()) {
    this.set(id, target, false);
  }

  /**
   * First paint, before the backend has answered. A built-in resolves
   * immediately; a user theme is painted from the cache so a light theme
   * does not flash dark on every launch.
   */
  bootstrap(id: string, target: StyleTarget | undefined = root()) {
    if (Object.hasOwn(BUILTIN_BY_ID, id)) {
      this.apply(id, target);
      return;
    }
    this.wantedId = id;
    const cached = readCache();
    if (cached?.id === id) {
      this.activeId = id;
      this.tokens = cached.tokens;
      if (target) applyTokens(target, cached.tokens);
      return;
    }
    // No usable cache for the wish. Show the default, but leave the cache
    // alone: it may still hold the tokens of another user theme, and this
    // paint is a placeholder, not the user's choice.
    this.paint(BUILTIN_BY_ID[DEFAULT_THEME_ID], target);
  }

  /**
   * Install the themes scanned from disk, then settle the wish against them.
   * This listing is authoritative: it honours a wish that has just become
   * resolvable, picks up an edited file for the theme already showing, and
   * drops back to the default when the wish names a file that is now gone —
   * cases `bootstrap` could only guess at.
   */
  setUserThemes(themes: Theme[], target: StyleTarget | undefined = root()) {
    this.user = themes;
    this.set(this.wantedId, target, true);
  }
}

export const theme = new ThemeStore();
