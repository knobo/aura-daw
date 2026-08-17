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

// A named colour escapes a hex-only check, and `color-mix(…, white 20%)` is
// every bit as unthemeable as `#ffffff` — worse in a light theme, where mixing
// toward white lowers the very contrast the mix was added to raise. Matched
// only in a value position (after `:`, `,` or `(`) and only inside <style>, so
// a `.black` selector or a `black ? …` predicate in the script is not a hit.
const NAMED = new RegExp(
  "[:,(]\\s*(white|black|red|green|blue|yellow|orange|purple|pink|violet" +
    "|cyan|magenta|gray|grey|silver|brown|gold|teal|navy|lime|olive|maroon" +
    "|aqua|fuchsia)(?=[\\s,)%;]|$)",
  "i",
);

const modules = import.meta.glob<string>("../components/**/*.svelte", {
  query: "?raw",
  import: "default",
  eager: true,
});

/** 1-based line numbers spanned by the component's <style> block. */
function styleLines(lines: string[]): { from: number; to: number } {
  const from = lines.findIndex((l) => l.includes("<style")) + 1;
  const to = lines.findIndex((l) => l.includes("</style>")) + 1;
  return { from: from || Infinity, to: to || 0 };
}

describe("components carry no colour literals", () => {
  const offenders = Object.entries(modules).flatMap(([path, content]) => {
    const lines = content.split("\n");
    const style = styleLines(lines);
    return lines
      .map((line, i) => ({ path, line, no: i + 1 }))
      .filter(
        ({ line, no }) =>
          (LITERAL.test(line) || (no > style.from && no < style.to && NAMED.test(line))) &&
          !EXEMPT.test(line),
      )
      .map(({ path, line, no }) => `${path}:${no}  ${line.trim()}`);
  });

  it("finds none — use a token, or annotate with /* theme-exempt: <reason> */", () => {
    expect(offenders).toEqual([]);
  });
});
