import { beforeEach, describe, expect, it, vi } from "vitest";
import { AURA_DARK } from "./builtins/index";
import type { Theme } from "./builtins/index";
import { theme, THEME_CACHE_KEY } from "./theme.svelte";

/** A stand-in user theme; Task 4 builds the real ones from JSON. */
function userTheme(id: string, patch: Partial<Theme["tokens"]>): Theme {
  return {
    id,
    name: id.toUpperCase(),
    base: "aura-dark",
    tokens: { ...AURA_DARK.tokens, ...patch },
    source: "user",
  };
}

const store: Record<string, string> = {};
vi.stubGlobal("localStorage", {
  getItem: (k: string) => store[k] ?? null,
  setItem: (k: string, v: string) => void (store[k] = v),
});

beforeEach(() => {
  for (const k of Object.keys(store)) delete store[k];
  theme.setUserThemes([]);
  theme.apply("aura-dark");
});

describe("apply", () => {
  it("swaps the reactive token set", () => {
    theme.setUserThemes([userTheme("sunny", { bg1: "#ffffff", text: "#000000" })]);
    theme.apply("sunny");
    expect(theme.activeId).toBe("sunny");
    expect(theme.tokens.bg1).toBe("#ffffff");
    expect(theme.tokens.text).toBe("#000000");
  });

  it("keeps base values the theme did not override", () => {
    theme.setUserThemes([userTheme("sunny", { bg1: "#ffffff" })]);
    theme.apply("sunny");
    expect(theme.tokens.cyan).toBe(AURA_DARK.tokens.cyan);
  });

  it("falls back to the default for an unknown id, rather than unstyling the app", () => {
    theme.apply("does-not-exist");
    expect(theme.activeId).toBe("aura-dark");
    expect(theme.tokens.bg1).toBe(AURA_DARK.tokens.bg1);
  });

  it("replaces the tokens object wholesale so canvas effects re-run exactly once", () => {
    const before = theme.tokens;
    theme.setUserThemes([userTheme("sunny", { bg1: "#ffffff" })]);
    theme.apply("sunny");
    expect(theme.tokens).not.toBe(before);
  });

  it("writes every custom property onto the supplied root", () => {
    const set = new Map<string, string>();
    theme.apply("aura-dark", { style: { setProperty: (k, v) => void set.set(k, v) } });
    expect(set.get("--bg-1")).toBe("#0a0d17");
    expect(set.get("--cyan-rgb")).toBe("82 229 255");
  });
});

describe("the boot cache", () => {
  it("persists the resolved tokens of the applied theme", () => {
    theme.setUserThemes([userTheme("sunny", { bg1: "#ffffff" })]);
    theme.apply("sunny");
    const cached = JSON.parse(store[THEME_CACHE_KEY]);
    expect(cached.id).toBe("sunny");
    expect(cached.tokens.bg1).toBe("#ffffff");
  });

  it("bootstrap paints a user theme from cache before the backend answers", () => {
    store[THEME_CACHE_KEY] = JSON.stringify({
      id: "sunny",
      tokens: { ...AURA_DARK.tokens, bg1: "#ffffff" },
    });
    theme.bootstrap("sunny");
    expect(theme.tokens.bg1).toBe("#ffffff");
  });

  it("bootstrap ignores a cache written for a different theme", () => {
    store[THEME_CACHE_KEY] = JSON.stringify({
      id: "other",
      tokens: { ...AURA_DARK.tokens, bg1: "#ffffff" },
    });
    theme.bootstrap("sunny");
    expect(theme.tokens.bg1).toBe(AURA_DARK.tokens.bg1);
  });

  it("bootstrap survives a corrupt cache", () => {
    store[THEME_CACHE_KEY] = "{not json";
    expect(() => theme.bootstrap("sunny")).not.toThrow();
    expect(theme.tokens.bg1).toBe(AURA_DARK.tokens.bg1);
  });

  // The cache outlives the build that wrote it. A shape check shallow enough
  // to pass a half-populated entry through to applyTokens throws inside the
  // mount effect, and boot stops there — before transport and project init.
  it("bootstrap ignores a cache that does not carry every token", () => {
    for (const tokens of [{}, { bg1: "#ffffff" }, { ...AURA_DARK.tokens, cyan: undefined }]) {
      store[THEME_CACHE_KEY] = JSON.stringify({ id: "sunny", tokens });
      const set = new Map<string, string>();
      expect(() =>
        theme.bootstrap("sunny", { style: { setProperty: (k, v) => void set.set(k, v) } }),
      ).not.toThrow();
      expect(theme.tokens.bg1).toBe(AURA_DARK.tokens.bg1);
      expect(set.get("--cyan")).toBe(AURA_DARK.tokens.cyan);
    }
  });

  it("bootstrap ignores a cache whose trackPalette is the wrong shape", () => {
    store[THEME_CACHE_KEY] = JSON.stringify({
      id: "sunny",
      tokens: { ...AURA_DARK.tokens, trackPalette: ["#ffffff"] },
    });
    theme.bootstrap("sunny");
    expect(theme.tokens.trackPalette).toEqual(AURA_DARK.tokens.trackPalette);
  });

  // App.svelte bootstraps in onMount and then applies prefs.values.theme in an
  // $effect. Both are user effects, so the apply lands immediately — long
  // before loadUserThemes() answers. It must not undo the cached paint, and it
  // must not overwrite the cache with the fallback, which would disable the
  // anti-flash path for every later launch too.
  it("the preference effect right after bootstrap leaves the cached paint alone", () => {
    store[THEME_CACHE_KEY] = JSON.stringify({
      id: "sunny",
      tokens: { ...AURA_DARK.tokens, bg1: "#ffffff" },
    });
    theme.bootstrap("sunny");
    theme.apply("sunny"); // the $effect, firing before the backend answers
    expect(theme.activeId).toBe("sunny");
    expect(theme.tokens.bg1).toBe("#ffffff");
    expect(JSON.parse(store[THEME_CACHE_KEY]).id).toBe("sunny");
  });

  it("bootstrap does not overwrite the cache with its placeholder paint", () => {
    const cached = JSON.stringify({ id: "sunny", tokens: { ...AURA_DARK.tokens, bg1: "#ffffff" } });
    store[THEME_CACHE_KEY] = cached;
    theme.bootstrap("other"); // a different wish: no usable cache, paints default
    expect(theme.tokens.bg1).toBe(AURA_DARK.tokens.bg1);
    expect(store[THEME_CACHE_KEY]).toBe(cached);
  });
});

describe("listing", () => {
  it("separates built-ins from user themes for the picker", () => {
    theme.setUserThemes([userTheme("sunny", {}), userTheme("mine", {})]);
    expect(theme.builtins().map((t) => t.id)).toContain("aura-dark");
    expect(theme.userThemes().map((t) => t.id)).toEqual(["sunny", "mine"]);
  });

  it("re-applies the active theme when its file arrives from the backend", () => {
    theme.apply("sunny"); // unknown → falls back, but remembers the wish
    expect(theme.tokens.bg1).toBe(AURA_DARK.tokens.bg1);
    theme.setUserThemes([userTheme("sunny", { bg1: "#ffffff" })]);
    expect(theme.tokens.bg1).toBe("#ffffff");
    expect(theme.activeId).toBe("sunny");
  });

  it("hands out a copy, so a caller cannot reorder the store's own list", () => {
    theme.setUserThemes([userTheme("sunny", {}), userTheme("mine", {})]);
    theme.userThemes().reverse();
    expect(theme.userThemes().map((t) => t.id)).toEqual(["sunny", "mine"]);
  });

  // The listing is authoritative: a wish painted from the cache but backed by
  // no file any more has to give way, or the app sits on tokens for a theme
  // the user deleted.
  it("drops back to the default when the wish's file is gone", () => {
    store[THEME_CACHE_KEY] = JSON.stringify({
      id: "sunny",
      tokens: { ...AURA_DARK.tokens, bg1: "#ffffff" },
    });
    theme.bootstrap("sunny");
    expect(theme.tokens.bg1).toBe("#ffffff");
    theme.setUserThemes([]); // the folder no longer holds sunny.json
    expect(theme.activeId).toBe("aura-dark");
    expect(theme.tokens.bg1).toBe(AURA_DARK.tokens.bg1);
  });
});
