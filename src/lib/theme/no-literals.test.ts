/**
 * The sweep's guard. Without it, the next component to land reintroduces a
 * hardcoded colour and the themes quietly stop covering the whole app.
 *
 * The escape hatch is a `theme-exempt` comment on the same line, for the
 * rare place a fixed colour is genuinely right — a brand mark, a colour that
 * carries meaning independent of the palette. It must state a reason.
 */
import { describe, expect, it } from "vitest";

const LITERAL = /#[0-9a-fA-F]{3,8}\b|rgba?\([0-9]/;
const EXEMPT = /theme-exempt:\s*\S/;

const modules = import.meta.glob<string>("../components/**/*.svelte", {
  query: "?raw",
  import: "default",
  eager: true,
});

describe("components carry no colour literals", () => {
  const offenders = Object.entries(modules).flatMap(([path, content]) =>
    content
      .split("\n")
      .map((line, i) => ({ path, line, no: i + 1 }))
      .filter(({ line }) => LITERAL.test(line) && !EXEMPT.test(line))
      .map(({ path, line, no }) => `${path}:${no}  ${line.trim()}`),
  );

  it("finds none — use a token, or annotate with /* theme-exempt: <reason> */", () => {
    expect(offenders).toEqual([]);
  });
});
