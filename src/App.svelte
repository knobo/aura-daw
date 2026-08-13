<script lang="ts">
  /**
   * AURA — DAW shell. Boots the stores against the backend adapter (real
   * Tauri engine, or the synthetic demo engine in a plain browser), wires
   * global shortcuts, and lays out transport / timeline / piano roll /
   * master chrome plus the right dock and the MCP confirm overlay.
   */
  import { onMount } from "svelte";
  import { transport } from "./lib/state/transport.svelte";
  import { project } from "./lib/state/project.svelte";
  import { midi } from "./lib/state/midi.svelte";
  import { instruments } from "./lib/state/instruments.svelte";
  import { plugins } from "./lib/state/plugins.svelte";
  import { mcp } from "./lib/state/mcp.svelte";
  import { startMeterStream, stopMeterStream } from "./lib/state/meters.svelte";
  import { view } from "./lib/state/view.svelte";
  import { ui } from "./lib/state/ui.svelte";
  import { exporter } from "./lib/state/exporter.svelte";
  import { loopjam } from "./lib/state/loopjam.svelte";
  import { generation } from "./lib/state/generation.svelte";
  import TransportBar from "./lib/components/TransportBar.svelte";
  import Timeline from "./lib/components/Timeline.svelte";
  import MasterBar from "./lib/components/MasterBar.svelte";
  import PianoRoll from "./lib/components/pianoroll/PianoRoll.svelte";
  import Dock from "./lib/components/Dock.svelte";
  import McpConfirmDialog from "./lib/components/mcp/McpConfirmDialog.svelte";
  import ExportDialog from "./lib/components/export/ExportDialog.svelte";
  import Toasts from "./lib/components/Toasts.svelte";

  onMount(() => {
    void transport.init();
    void project.init();
    void midi.init();
    void instruments.refresh();
    void plugins.refresh();
    void mcp.init();
    exporter.init();
    void loopjam.init();
    generation.init(); // adopt jobs an agent starts over MCP
    void startMeterStream();
    return () => {
      stopMeterStream();
      generation.dispose();
    };
  });

  function onKeydown(e: KeyboardEvent) {
    const tag = (e.target as HTMLElement)?.tagName;
    if (tag === "INPUT" || tag === "SELECT" || tag === "TEXTAREA") return;
    // piano roll owns its own keys while hovered/focused
    if ((e.target as HTMLElement)?.closest?.(".roll")) return;
    if (e.code === "Space") {
      e.preventDefault();
      void transport.togglePlay();
    } else if (e.key === "Home") {
      void transport.seek(0);
    } else if (e.key === "+" || e.key === "=") {
      view.zoomAt(view.width / 2, 0.75);
    } else if (e.key === "-") {
      view.zoomAt(view.width / 2, 1.33);
    } else if (e.key.toLowerCase() === "s" && !e.metaKey && !e.ctrlKey) {
      view.snap = !view.snap;
    } else if (e.key.toLowerCase() === "g" && !e.metaKey && !e.ctrlKey) {
      ui.dock = ui.dock === "generate" ? "" : "generate";
    }
  }
</script>

<svelte:window onkeydown={onKeydown} />

<div class="app">
  <TransportBar />
  <div class="main">
    <Timeline />
    <Dock />
  </div>
  <PianoRoll />
  <MasterBar />
</div>

<McpConfirmDialog />
<ExportDialog />
<Toasts />

<style>
  .app {
    height: 100vh;
    display: flex;
    flex-direction: column;
    /* clip (not hidden): focus moves must never scroll the app shell */
    overflow: clip;
  }
  .main {
    flex: 1;
    min-height: 0;
    position: relative;
    display: flex;
    flex-direction: column;
  }
</style>
