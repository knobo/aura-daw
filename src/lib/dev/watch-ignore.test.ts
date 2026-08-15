/**
 * The dev server's watch scope, checked through picomatch — the matcher
 * chokidar (and so Vite) actually applies to `server.watch.ignored`. Asserting
 * the pattern STRINGS would pass for a glob that never matches anything; these
 * assert against the paths that really did reload a live session.
 */
import { describe, expect, it } from "vitest";
import picomatch from "picomatch";
import { DEV_WATCH_IGNORED } from "./watch-ignore";

const isIgnored = picomatch(DEV_WATCH_IGNORED);
const ROOT = "/home/u/prog/dav";

describe("dev server watch scope", () => {
  it("ignores everything under agent worktrees", () => {
    // The three shapes observed reloading a live session: another worktree's
    // build output, its sources, and its tsconfig — the last one makes Vite
    // clear its cache and force a reload on every connected client.
    expect(isIgnored(`${ROOT}/.claude/worktrees/some-branch/dist/index.html`)).toBe(true);
    expect(isIgnored(`${ROOT}/.claude/worktrees/some-branch/src/App.svelte`)).toBe(true);
    expect(isIgnored(`${ROOT}/.claude/worktrees/some-branch/tsconfig.json`)).toBe(true);
  });

  it("ignores build output and the Rust side", () => {
    expect(isIgnored(`${ROOT}/dist/index.html`)).toBe(true);
    expect(isIgnored(`${ROOT}/src-tauri/src/lib.rs`)).toBe(true);
  });

  it("still watches the app's own frontend sources", () => {
    // Over-broad ignores would cost HMR, which is the point of the dev server.
    expect(isIgnored(`${ROOT}/src/App.svelte`)).toBe(false);
    expect(isIgnored(`${ROOT}/src/lib/state/project.svelte.ts`)).toBe(false);
    expect(isIgnored(`${ROOT}/index.html`)).toBe(false);
  });
});
