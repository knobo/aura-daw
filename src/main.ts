import { mount } from "svelte";
import "./app.css";
import App from "./App.svelte";
import {
  GROUP_HEADER_PX,
  LANE_COLLAPSED_PX,
  LANE_HEIGHT_PX,
  RAIL_WIDTH_PX,
} from "./lib/utils/lane-layout";

// Publish the shared geometry constants before mount. app.css carries only
// matching fallbacks for non-app renderers; lane-layout is authoritative.
document.documentElement.style.setProperty(
  "--track-height",
  String(LANE_HEIGHT_PX) + "px",
);
document.documentElement.style.setProperty(
  "--rail-width",
  String(RAIL_WIDTH_PX) + "px",
);
document.documentElement.style.setProperty(
  "--lane-collapsed",
  String(LANE_COLLAPSED_PX) + "px",
);
document.documentElement.style.setProperty(
  "--lane-group-height",
  String(GROUP_HEADER_PX) + "px",
);

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
