/**
 * What the Vite dev server must NOT watch, given the root it is serving.
 *
 * This is correctness, not tidiness: a watched path that is not an input to
 * the running app still triggers a full page reload, and a full reload throws
 * away every in-memory store — the scanned plugin list, loaded Zyn patches,
 * the open project's UI state — with nothing on screen to say why.
 *
 * Lives here rather than inline in `vite.config.ts` so it can be tested: the
 * config file is outside `tsconfig.json`'s `include`, and pulling it into the
 * type-check graph drags in Node globals the app does not type.
 */

/** Where agent worktrees are checked out, relative to the repo root. */
const WORKTREES = ".claude/worktrees";

/**
 * `root` is the directory Vite serves (`process.cwd()` by default). It decides
 * one thing that a fixed pattern list cannot: agent worktrees sit INSIDE the
 * repo root, so `**\/.claude/worktrees/**` ignores another worktree's build
 * when the dev server runs from the primary checkout — and ignores THE APP
 * ITSELF when the dev server runs from inside a worktree, which is exactly how
 * `.claude/skills/run-aura` starts it. That second case cost every file its
 * watcher, silently: no HMR, no reload, no error. So the worktree pattern is
 * anchored to this root, and dropped entirely when the root is a worktree
 * (nothing above the root is watched in the first place).
 */
export function devWatchIgnored(root: string): string[] {
  const normalized = root.replace(/\\/g, "/").replace(/\/+$/, "");
  const servingAWorktree = normalized.includes(`/${WORKTREES}/`);
  return [
    // The Rust side. Cargo rebuilds are Tauri's business, not Vite's.
    "**/src-tauri/**",
    // Build output. Nothing here is an input to the dev server, so a
    // `npm run build` should not reload the app someone is testing.
    "**/dist/**",
    // Sibling worktrees under the primary checkout. A worktree's tsconfig is
    // the worst of these: Vite answers it by clearing its cache and forcing a
    // reload on every connected client.
    ...(servingAWorktree ? [] : [`${normalized}/${WORKTREES}/**`]),
  ];
}
