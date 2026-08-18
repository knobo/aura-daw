import { defineConfig } from "vitest/config";
import { svelte } from "@sveltejs/vite-plugin-svelte";

// Two projects because `.svelte` files compile differently per target:
// `unit` runs stores/utils in plain node against the SSR-compiled output
// (no DOM, no `mount()`); `dom` runs *.dom.test.ts under jsdom against the
// client-compiled output (needs `resolve.conditions: ["browser"]` or
// vite-plugin-svelte keeps emitting the SSR build and `mount()` throws
// "not available on the server").
export default defineConfig({
  test: {
    projects: [
      {
        plugins: [svelte()],
        test: {
          name: "unit",
          environment: "node",
          include: ["src/**/*.test.ts"],
          exclude: ["src/**/*.dom.test.ts"],
        },
      },
      {
        resolve: { conditions: ["browser"] },
        plugins: [svelte()],
        test: {
          name: "dom",
          environment: "jsdom",
          include: ["src/**/*.dom.test.ts"],
        },
      },
    ],
  },
});
