import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import tailwindcss from "@tailwindcss/vite";
import { devWatchIgnored } from "./src/lib/dev/watch-ignore";

// Tauri expects a fixed dev port; fail rather than pick a random one.
const host = process.env.TAURI_DEV_HOST;

/**
 * Ensures Svelte virtual CSS modules (?svelte&type=style) load their compiled CSS
 * even if requested before the parent Svelte component has been transformed in Vite.
 *
 * Without this, Vite's fallback loads the raw .svelte file from disk and passes its
 * script/markup to Tailwind's CSS parser, causing "Invalid declaration: <js_ident>".
 */
function svelteVirtualCssFix(): import("vite").Plugin {
  return {
    name: "svelte-virtual-css-fix",
    enforce: "pre",
    async load(id) {
      if (id.includes("?svelte&type=style")) {
        const [filename] = id.split("?", 2);
        let cachedCss = this.getModuleInfo(filename)?.meta?.svelte?.css;
        if (!cachedCss) {
          await this.load({ id: filename });
          cachedCss = this.getModuleInfo(filename)?.meta?.svelte?.css;
        }
        if (cachedCss) {
          const { hasGlobal, ...css } = cachedCss;
          if (hasGlobal === false) {
            css.meta ??= {};
            css.meta.vite ??= {};
            css.meta.vite.cssScopeTo = [filename, "default"];
          }
          css.moduleType = "css";
          return css;
        }
        return {
          code: "",
          map: null,
          moduleType: "css",
        };
      }
    },
  };
}

export default defineConfig({
  plugins: [svelteVirtualCssFix(), tailwindcss(), svelte()],

  // Vite options tailored for Tauri development.
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    // Patterns live in src/lib/dev/watch-ignore.ts so they can be tested.
    // `process.cwd()` is the root Vite serves by default, and the worktree
    // rule depends on it — see that module.
    watch: { ignored: devWatchIgnored(process.cwd()) },
  },
  // Env variables starting with these prefixes are exposed to the frontend.
  envPrefix: ["VITE_", "TAURI_ENV_"],
  build: {
    target: "es2022",
    minify: !process.env.TAURI_ENV_DEBUG ? "esbuild" : false,
    sourcemap: !!process.env.TAURI_ENV_DEBUG,
  },
});
