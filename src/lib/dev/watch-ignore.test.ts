/**
 * The dev server's watch scope. The case that matters most is the second one:
 * a blanket `**\/.claude/worktrees/**` also matches the app's own files when
 * the dev server is started from inside a worktree, which silently costs every
 * file its watcher.
 */
import { describe, expect, it } from "vitest";
import { devWatchIgnored } from "./watch-ignore";

const CHECKOUT = "/home/u/prog/dav";
const WORKTREE = `${CHECKOUT}/.claude/worktrees/some-branch`;

describe("dev server watch scope", () => {
  it("ignores sibling worktrees when serving the primary checkout", () => {
    // Anchored to this root, not a floating `**/` prefix — see below for why.
    expect(devWatchIgnored(CHECKOUT)).toContain(`${CHECKOUT}/.claude/worktrees/**`);
  });

  it("does NOT ignore worktrees when the served root IS a worktree", () => {
    // Otherwise the pattern matches this worktree's own src/, index.html and
    // config: no HMR, no reload, no error message. Nothing above the root is
    // watched anyway, so sibling worktrees need no pattern here.
    const patterns = devWatchIgnored(WORKTREE);
    expect(patterns.some((p) => p.includes(".claude/worktrees"))).toBe(false);
  });

  it("ignores build output and the Rust side from either root", () => {
    for (const root of [CHECKOUT, WORKTREE]) {
      expect(devWatchIgnored(root)).toContain("**/src-tauri/**");
      expect(devWatchIgnored(root)).toContain("**/dist/**");
    }
  });

  it("tolerates a trailing separator and Windows separators in the root", () => {
    expect(devWatchIgnored(`${CHECKOUT}/`)).toContain(`${CHECKOUT}/.claude/worktrees/**`);
    expect(devWatchIgnored("C:\\u\\dav")).toContain("C:/u/dav/.claude/worktrees/**");
    expect(
      devWatchIgnored("C:\\u\\dav\\.claude\\worktrees\\b").some((p) =>
        p.includes(".claude/worktrees"),
      ),
    ).toBe(false);
  });
});
