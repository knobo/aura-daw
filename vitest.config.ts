import { defineConfig } from "vitest/config";
import { svelte } from "@sveltejs/vite-plugin-svelte";

// Frontend unit tests. The svelte plugin is what compiles runes in
// `*.svelte.ts` store modules; no DOM is needed since these test stores,
// not components.
export default defineConfig({
  plugins: [svelte()],
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
  },
});
