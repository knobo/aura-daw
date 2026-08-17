import { describe, expect, it } from "vitest";
import { AURA_DARK } from "./builtins/index";
import { parseUserTheme } from "./parse";

const json = (o: unknown) => JSON.stringify(o);

describe("whole-file rejection", () => {
  it("rejects unparseable JSON", () => {
    const r = parseUserTheme("mine", "{not json");
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.reason).toMatch(/JSON/i);
  });

  it("rejects a non-object top level", () => {
    expect(parseUserTheme("mine", json([1, 2, 3])).ok).toBe(false);
    expect(parseUserTheme("mine", json("hello")).ok).toBe(false);
  });

  it("rejects a missing or blank name", () => {
    expect(parseUserTheme("mine", json({ tokens: {} })).ok).toBe(false);
    expect(parseUserTheme("mine", json({ name: "   " })).ok).toBe(false);
  });

  it("rejects an extends that names something that is not a built-in", () => {
    const r = parseUserTheme("mine", json({ name: "Mine", extends: "nonesuch" }));
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.reason).toMatch(/nonesuch/);
  });

  it("rejects an id that collides with a built-in, rather than shadowing it", () => {
    const r = parseUserTheme("aura-dark", json({ name: "Impostor" }));
    expect(r.ok).toBe(false);
    if (!r.ok) expect(r.reason).toMatch(/built-in/i);
  });

  // On a plain object every one of these reads back truthy, which is enough
  // to pass an `extends` check and register a theme with NO tokens at all.
  it("does not mistake a prototype key for a built-in", () => {
    for (const ext of ["__proto__", "constructor", "toString", "valueOf"]) {
      const r = parseUserTheme("mine", json({ name: "Mine", extends: ext }));
      expect(r.ok, ext).toBe(false);
    }
  });

  // The flip side: a prototype key is a legal FILENAME, so a theme called
  // constructor.json must load like any other rather than being turned away
  // as a built-in.
  it("accepts a prototype key as a theme id", () => {
    const r = parseUserTheme("constructor", json({ name: "Mine" }));
    expect(r.ok).toBe(true);
    if (r.ok) expect(r.theme.tokens.cyan).toBe(AURA_DARK.tokens.cyan);
  });
});

describe("successful parse", () => {
  it("defaults extends to aura-dark and copies the base when tokens are absent", () => {
    const r = parseUserTheme("mine", json({ name: "Mine" }));
    expect(r.ok).toBe(true);
    if (!r.ok) return;
    expect(r.theme.id).toBe("mine");
    expect(r.theme.name).toBe("Mine");
    expect(r.theme.base).toBe("aura-dark");
    expect(r.theme.source).toBe("user");
    expect(r.theme.tokens).toEqual(AURA_DARK.tokens);
  });

  it("overlays only the keys the file names", () => {
    const r = parseUserTheme("mine", json({ name: "Mine", tokens: { cyan: "#268bd2" } }));
    expect(r.ok).toBe(true);
    if (!r.ok) return;
    expect(r.theme.tokens.cyan).toBe("#268bd2");
    expect(r.theme.tokens.magenta).toBe(AURA_DARK.tokens.magenta);
  });

  it("accepts every colour spelling a person is likely to type", () => {
    const r = parseUserTheme(
      "mine",
      json({ name: "Mine", tokens: { cyan: "#abc", red: "#aabbccdd", green: "rgb(1, 2, 3)", violet: "rgba(1,2,3,0.5)" } }),
    );
    expect(r.ok).toBe(true);
    if (!r.ok) return;
    expect(r.dropped).toEqual([]);
  });

  it("accepts affordance lengths and the unitless affordances", () => {
    const r = parseUserTheme(
      "mine",
      json({ name: "Mine", tokens: { borderWidth: "2px", glassBlur: "0px", focusWidth: "0.2rem", bodyGlow: "0", glassAlpha: "1", glowScale: "0.5" } }),
    );
    expect(r.ok).toBe(true);
    if (!r.ok) return;
    expect(r.theme.tokens.borderWidth).toBe("2px");
    expect(r.theme.tokens.bodyGlow).toBe("0");
    expect(r.theme.tokens.glassAlpha).toBe("1");
    expect(r.theme.tokens.glowScale).toBe("0.5");
  });

  it("drops a unitless affordance written as a length", () => {
    const r = parseUserTheme("mine", json({ name: "Mine", tokens: { glowScale: "6px" } }));
    expect(r.ok).toBe(true);
    if (!r.ok) return;
    expect(r.theme.tokens.glowScale).toBe(AURA_DARK.tokens.glowScale);
    expect(r.dropped).toContain("glowScale");
  });

  it("takes a six-colour trackPalette", () => {
    const p = ["#111111", "#222222", "#333333", "#444444", "#555555", "#666666"];
    const r = parseUserTheme("mine", json({ name: "Mine", tokens: { trackPalette: p } }));
    expect(r.ok).toBe(true);
    if (!r.ok) return;
    expect(r.theme.tokens.trackPalette).toEqual(p);
  });
});

describe("key-level degradation — a bad key must not lose the whole theme", () => {
  it("drops an unknown key and reports it", () => {
    const r = parseUserTheme("mine", json({ name: "Mine", tokens: { cyan: "#268bd2", nonsense: "#fff" } }));
    expect(r.ok).toBe(true);
    if (!r.ok) return;
    expect(r.theme.tokens.cyan).toBe("#268bd2");
    expect(r.dropped).toContain("nonsense");
  });

  it("drops a malformed colour and keeps the base value", () => {
    const r = parseUserTheme("mine", json({ name: "Mine", tokens: { cyan: "not-a-colour" } }));
    expect(r.ok).toBe(true);
    if (!r.ok) return;
    expect(r.theme.tokens.cyan).toBe(AURA_DARK.tokens.cyan);
    expect(r.dropped).toContain("cyan");
  });

  // A colour the validator accepts but `channels()` cannot read is worse than
  // a rejected one: it reports nothing dropped and then emits `NaN NaN NaN`.
  it("drops rgb() spellings the token layer cannot parse", () => {
    for (const cyan of ["rgb(80% 50% 20%)", "rgb(none none none)", "rgb(abc)", "rgb(1 2)"]) {
      const r = parseUserTheme("mine", json({ name: "Mine", tokens: { cyan } }));
      expect(r.ok, cyan).toBe(true);
      if (!r.ok) continue;
      expect(r.dropped, cyan).toContain("cyan");
      expect(r.theme.tokens.cyan, cyan).toBe(AURA_DARK.tokens.cyan);
    }
  });

  it("still accepts the space- and comma-separated numeric forms", () => {
    const r = parseUserTheme(
      "mine",
      json({ name: "Mine", tokens: { cyan: "rgb(82 229 255)", red: "rgb(82 229 255 / 0.4)" } }),
    );
    expect(r.ok).toBe(true);
    if (r.ok) expect(r.dropped).toEqual([]);
  });

  it("drops a non-string value", () => {
    const r = parseUserTheme("mine", json({ name: "Mine", tokens: { cyan: 42 } }));
    expect(r.ok).toBe(true);
    if (!r.ok) return;
    expect(r.dropped).toContain("cyan");
  });

  it("drops an affordance that is not a CSS length", () => {
    const r = parseUserTheme("mine", json({ name: "Mine", tokens: { borderWidth: "thick" } }));
    expect(r.ok).toBe(true);
    if (!r.ok) return;
    expect(r.theme.tokens.borderWidth).toBe(AURA_DARK.tokens.borderWidth);
    expect(r.dropped).toContain("borderWidth");
  });

  it("drops a trackPalette of the wrong length or with a bad entry", () => {
    const short = parseUserTheme("mine", json({ name: "Mine", tokens: { trackPalette: ["#111111"] } }));
    expect(short.ok).toBe(true);
    if (short.ok) expect(short.dropped).toContain("trackPalette");

    const bad = parseUserTheme(
      "mine",
      json({ name: "Mine", tokens: { trackPalette: ["#111111", "nope", "#333333", "#444444", "#555555", "#666666"] } }),
    );
    expect(bad.ok).toBe(true);
    if (bad.ok) expect(bad.dropped).toContain("trackPalette");
  });

  it("rejects a non-object tokens map as a whole", () => {
    expect(parseUserTheme("mine", json({ name: "Mine", tokens: "red" })).ok).toBe(false);
  });
});
