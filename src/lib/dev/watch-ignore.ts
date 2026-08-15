/**
 * What the Vite dev server must NOT watch.
 *
 * This is correctness, not tidiness: a watched path that is not an input to
 * the running app still triggers a full page reload, and a full reload throws
 * away every in-memory store — the scanned plugin list, loaded Zyn patches,
 * the open project's UI state — with nothing on screen to say why.
 *
 * Lives here rather than inline in `vite.config.ts` so it can be tested: the
 * config file itself is outside `tsconfig.json`'s `include`, and pulling it
 * into the type-check graph drags in Node globals the app does not type.
 */
export const DEV_WATCH_IGNORED = [
  // The Rust side. Cargo rebuilds are Tauri's business, not Vite's.
  "**/src-tauri/**",
  // Agent worktrees. These live INSIDE the repo root Vite watches, so a
  // build in any of them reloaded the app running from the primary
  // checkout, mid-session. A worktree's tsconfig was worse still: Vite
  // clears its cache and forces a reload on every connected client.
  "**/.claude/worktrees/**",
  // Build output. Nothing here is an input to the dev server, so a
  // `npm run build` should not reload the app someone is testing.
  "**/dist/**",
];
