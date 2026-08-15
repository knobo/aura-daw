/**
 * picomatch is the glob matcher chokidar — and therefore Vite's watcher —
 * runs `server.watch.ignored` through. It reaches us as a transitive dep and
 * ships no types, and this repo keeps no `@types/*` in its devDependencies,
 * so declare the one function `watch-ignore.test.ts` needs rather than add a
 * dependency for a single test.
 */
declare module "picomatch" {
  export default function picomatch(
    patterns: string | string[],
  ): (candidate: string) => boolean;
}
