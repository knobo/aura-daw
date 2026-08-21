import { describe, expect, it } from "vitest";
import {
  AFFORDANCE_KEYS,
  MATERIAL_KEYS,
  TOKEN_KEYS,
  UNITLESS_AFFORDANCE_KEYS,
  alpha,
  applyTokens,
  rgbTriple,
  toCssVars,
  type ThemeTokens,
} from "./tokens";

/** A minimal, fully-populated token set — every test here uses this. */
const T: ThemeTokens = {
  bgVoid: "#030408",
  bg0: "#05070d",
  bgSunken: "#080a13",
  bg1: "#0a0d17",
  bg2: "#10142a",
  bg3: "#1b2340",
  glass: "#0d111e",
  line: "#6082be",
  edge: "#7aa0dc",
  cyan: "#52e5ff",
  cyanBright: "#8ef0ff",
  cyanDeep: "#1e7f95",
  magenta: "#ff4fd8",
  amber: "#ffc857",
  amberSunken: "#1a1408",
  red: "#ff4152",
  redSoft: "#ff8b96",
  violet: "#9d7bff",
  green: "#5cf2b8",
  orange: "#ff8b5c",
  text: "#d8e3f2",
  textMid: "#8fa3c4",
  textDim: "#5f6c85",
  textFaint: "#39435c",
  textOnAccent: "#ffffff",
  shadow: "#000000",
  trackPalette: ["#52e5ff", "#ff4fd8", "#ffc857", "#9d7bff", "#5cf2b8", "#ff8b5c"],
  borderWidth: "1px",
  focusWidth: "1px",
  glassBlur: "18px",
  glassAlpha: "0.62",
  glowScale: "1",
  bodyGlow: "0.05",
  panelAlpha: "0.88",
  bevel: "0.4",
  relief: "0.55",
  sheen: "0.3",
  grain: "0.1",
  ctrlRadius: "6px",
};

describe("rgbTriple", () => {
  it("renders a space-separated triple for #rrggbb", () => {
    expect(rgbTriple("#52e5ff")).toBe("82 229 255");
  });

  it("expands #rgb shorthand", () => {
    expect(rgbTriple("#fff")).toBe("255 255 255");
  });

  it("ignores the alpha channel of #rrggbbaa", () => {
    expect(rgbTriple("#52e5ff80")).toBe("82 229 255");
  });

  it("accepts rgb() and rgba() input", () => {
    expect(rgbTriple("rgba(96, 130, 190, 0.4)")).toBe("96 130 190");
  });
});

describe("alpha", () => {
  it("applies an alpha to a hex colour", () => {
    expect(alpha("#52e5ff", 0.4)).toBe("rgb(82 229 255 / 0.4)");
  });

  it("multiplies through the alpha already carried by #rrggbbaa", () => {
    // 0x80 = 128/255 = 0.50196…; times 0.5 = 0.25098…, rounded to 3 places.
    expect(alpha("#52e5ff80", 0.5)).toBe("rgb(82 229 255 / 0.251)");
  });

  it("clamps out-of-range alpha", () => {
    expect(alpha("#000000", 2)).toBe("rgb(0 0 0 / 1)");
  });
});

describe("toCssVars", () => {
  const vars = toCssVars(T);

  it("emits every colour token in both plain and -rgb form", () => {
    expect(vars["--cyan"]).toBe("#52e5ff");
    expect(vars["--cyan-rgb"]).toBe("82 229 255");
    expect(vars["--bg-void"]).toBe("#030408");
    expect(vars["--bg-void-rgb"]).toBe("3 4 8");
    expect(vars["--text-on-accent"]).toBe("#ffffff");
  });

  it("emits the six track colours as --track-N", () => {
    expect(vars["--track-1"]).toBe("#52e5ff");
    expect(vars["--track-6"]).toBe("#ff8b5c");
    expect(vars["--track-6-rgb"]).toBe("255 139 92");
    expect(vars["--track-7"]).toBeUndefined();
  });

  it("emits affordances verbatim", () => {
    expect(vars["--border-width"]).toBe("1px");
    expect(vars["--focus-width"]).toBe("1px");
    expect(vars["--glass-blur"]).toBe("18px");
    expect(vars["--glass-alpha"]).toBe("0.62");
    expect(vars["--glow-scale"]).toBe("1");
    expect(vars["--body-glow"]).toBe("0.05");
  });

  it("emits the derived tokens app.css already exposes, as literal colours", () => {
    expect(vars["--glass-base-rgb"]).toBe("13 17 30");
    expect(vars["--glass"]).toBe("rgb(13 17 30 / 0.62)");
    expect(toCssVars({ ...T, glassAlpha: "1" })["--glass"]).toBe("rgb(13 17 30 / 1)");
    expect(vars["--glass-border"]).toBe("rgb(122 160 220 / 0.12)");
    expect(vars["--grid-line"]).toBe("rgb(96 130 190 / 0.09)");
    expect(vars["--grid-line-strong"]).toBe("rgb(96 130 190 / 0.2)");
    expect(vars["--cyan-dim"]).toBe("rgb(82 229 255 / 0.35)");
    expect(vars["--magenta-dim"]).toBe("rgb(255 79 216 / 0.35)");
  });
});

describe("applyTokens", () => {
  it("sets every emitted variable on the target's style", () => {
    const set = new Map<string, string>();
    applyTokens({ style: { setProperty: (k, v) => void set.set(k, v) } }, T);
    expect(set.get("--cyan")).toBe("#52e5ff");
    expect(set.get("--cyan-rgb")).toBe("82 229 255");
    expect(set.size).toBe(Object.keys(toCssVars(T)).length);
  });
});

describe("the material tokens", () => {
  const vars = toCssVars(T);

  // The material scalars ride the same affordance path as `glowScale`: they
  // are emitted verbatim, and app.css does the arithmetic. If they ever
  // acquired an `-rgb` twin it would mean they had been misfiled as colours.
  it("emits each material scalar as a bare custom property", () => {
    expect(vars["--bevel"]).toBe("0.4");
    expect(vars["--relief"]).toBe("0.55");
    expect(vars["--sheen"]).toBe("0.3");
    expect(vars["--grain"]).toBe("0.1");
    expect(vars["--ctrl-radius"]).toBe("6px");
  });

  it("does not treat them as colours", () => {
    for (const key of ["--bevel", "--relief", "--sheen", "--grain", "--ctrl-radius"]) {
      expect(vars[key + "-rgb"], key).toBeUndefined();
    }
  });

  it("lists every material key as an affordance, so parse.ts validates them", () => {
    for (const key of MATERIAL_KEYS) {
      expect(AFFORDANCE_KEYS as readonly string[]).toContain(key);
      expect(TOKEN_KEYS as readonly string[]).toContain(key);
    }
  });

  // A theme file may say `"bevel": "0.5"` but never `"bevel": "0.5px"`, and
  // `ctrlRadius` is the one that goes the other way. Getting this backwards
  // would make every material override in a user theme silently drop.
  it("counts the four strengths as unitless and the radius as a length", () => {
    expect(UNITLESS_AFFORDANCE_KEYS).toContain("bevel");
    expect(UNITLESS_AFFORDANCE_KEYS).toContain("relief");
    expect(UNITLESS_AFFORDANCE_KEYS).toContain("sheen");
    expect(UNITLESS_AFFORDANCE_KEYS).toContain("grain");
    expect(UNITLESS_AFFORDANCE_KEYS).not.toContain("ctrlRadius");
  });
});

describe("the chrome panel fill", () => {
  it("mixes the panel colour at the theme's panel alpha", () => {
    expect(toCssVars({ ...T, panelAlpha: "0.5" })["--panel"]).toBe("rgb(13 17 30 / 0.5)");
  });

  /**
   * The pairing `glassBlur`/`glassAlpha` needs a test to police is DERIVED
   * here instead, so a theme cannot get it wrong: a backdrop-filter behind a
   * fully opaque surface blurs pixels nobody can see, and the dock is the
   * largest always-on surface in the app.
   */
  it("drops the blur once the panel is opaque", () => {
    expect(toCssVars({ ...T, panelAlpha: "1", glassBlur: "18px" })["--panel-blur"]).toBe("0px");
  });

  it("keeps the blur while the panel is see-through", () => {
    expect(toCssVars({ ...T, panelAlpha: "0.88", glassBlur: "18px" })["--panel-blur"]).toBe("18px");
  });

  it("treats a nonsense alpha as opaque rather than emitting NaN", () => {
    expect(toCssVars({ ...T, panelAlpha: "" })["--panel-blur"]).toBe("0px");
  });
});
