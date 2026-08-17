import { mount } from "svelte";
import "./app.css";
import App from "./App.svelte";
import { GROUP_HEADER_PX, LANE_COLLAPSED_PX } from "./lib/utils/lane-layout";

// A folded lane and a group header are the same height on purpose (see the
// doc comments on LANE_COLLAPSED_PX / GROUP_HEADER_PX) — set here, before
// anything mounts, so app.css doesn't carry its own copy of the number.
document.documentElement.style.setProperty("--lane-collapsed", `${LANE_COLLAPSED_PX}px`);
document.documentElement.style.setProperty("--lane-group-height", `${GROUP_HEADER_PX}px`);

const app = mount(App, {
  target: document.getElementById("app")!,
});

// Dev aid: expose the live singletons for console poking / UI automation.
// (Harmless in production — same objects the UI already holds.)
if (import.meta.env.DEV) {
  void (async () => {
    (await import("./lib/dev-drive")).startDevDrive();
    (window as unknown as Record<string, unknown>).__aura = {
      backend: (await import("./lib/tauri")).backend,
      project: (await import("./lib/state/project.svelte")).project,
      midi: (await import("./lib/state/midi.svelte")).midi,
      instruments: (await import("./lib/state/instruments.svelte")).instruments,
      mcp: (await import("./lib/state/mcp.svelte")).mcp,
      generation: (await import("./lib/state/generation.svelte")).generation,
    };
  })();
}

export default app;
