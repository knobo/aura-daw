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

import { applyTokens, type StyleTarget, type ThemeTokens } from "./tokens";
import { BUILTIN_BY_ID, BUILTIN_THEMES, DEFAULT_THEME_ID, type Theme } from "./builtins/index";

/** Last applied theme, so a user theme paints correctly on the next boot
 * before the backend has listed the themes folder. */
export const THEME_CACHE_KEY = "aura.theme.cache";

function root(): StyleTarget | undefined {
  return typeof document === "undefined" ? undefined : document.documentElement;
}

function readCache(): { id: string; tokens: ThemeTokens } | undefined {
  try {
    const parsed: unknown = JSON.parse(localStorage.getItem(THEME_CACHE_KEY) ?? "");
    if (parsed && typeof parsed === "object" && "id" in parsed && "tokens" in parsed) {
      return parsed as { id: string; tokens: ThemeTokens };
    }
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
    return this.user;
  }

  /** The theme for an id, or the default when the id is unknown. */
  resolve(id: string): Theme {
    return BUILTIN_BY_ID[id] ?? this.user.find((t) => t.id === id) ?? BUILTIN_BY_ID[DEFAULT_THEME_ID];
  }

  /**
   * Paint a theme. An unknown id degrades to the default rather than
   * leaving the app unstyled — the same contract `coercePref` follows.
   */
  apply(id: string, target: StyleTarget | undefined = root()) {
    this.wantedId = id;
    const resolved = this.resolve(id);
    this.activeId = resolved.id;
    this.tokens = resolved.tokens;
    if (target) applyTokens(target, resolved.tokens);
    writeCache(resolved.id, resolved.tokens);
  }

  /**
   * First paint, before the backend has answered. A built-in resolves
   * immediately; a user theme is painted from the cache so a light theme
   * does not flash dark on every launch.
   */
  bootstrap(id: string, target: StyleTarget | undefined = root()) {
    if (BUILTIN_BY_ID[id]) {
      this.apply(id, target);
      return;
    }
    const cached = readCache();
    this.wantedId = id;
    if (cached?.id === id) {
      this.activeId = id;
      this.tokens = cached.tokens;
      if (target) applyTokens(target, cached.tokens);
      return;
    }
    this.apply(DEFAULT_THEME_ID, target);
    this.wantedId = id;
  }

  /** Install the themes scanned from disk, then honour any pending wish. */
  setUserThemes(themes: Theme[], target: StyleTarget | undefined = root()) {
    this.user = themes;
    if (this.wantedId !== this.activeId || this.user.some((t) => t.id === this.activeId)) {
      this.apply(this.wantedId, target);
    }
  }
}

export const theme = new ThemeStore();
