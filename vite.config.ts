import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";
import tailwindcss from "@tailwindcss/vite";
import { devWatchIgnored } from "./src/lib/dev/watch-ignore";

// Tauri expects a fixed dev port; fail rather than pick a random one.
const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  // tailwindcss() BEFORE svelte(): in the other order Tailwind's transform can
  // reach a .svelte virtual css module before vite-plugin-svelte has loaded it,
  // then parses the component's JS as CSS ("Invalid declaration: `onMount`").
  plugins: [tailwindcss(), svelte()],

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
