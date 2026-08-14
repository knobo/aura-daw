<script lang="ts">
  /**
   * Right dock: slide-in glass drawer hosting AI Studio, the instrument
   * browser and the MCP control panel. Overlays the timeline; toggled from
   * the transport bar chips.
   */
  import { ui, type DockTab } from "../state/ui.svelte";
  import { DOCK_RESIZE } from "../utils/panel-resize";
  import PanelResizeHandle from "./PanelResizeHandle.svelte";
  import { mcp } from "../state/mcp.svelte";
  import { plugins } from "../state/plugins.svelte";
  import { zyn } from "../state/zynpatches.svelte";
  import AiStudio from "./generate/AiStudio.svelte";
  import HumPanel from "./hum/HumPanel.svelte";
  import LibraryPanel from "./library/LibraryPanel.svelte";
  import InstrumentBrowser from "./instruments/InstrumentBrowser.svelte";
  import PluginBrowser from "./plugins/PluginBrowser.svelte";
  import PluginParamPanel from "./plugins/PluginParamPanel.svelte";
  import ZynPatchBrowser from "./plugins/ZynPatchBrowser.svelte";
  import McpPanel from "./mcp/McpPanel.svelte";

  const TABS: { id: Exclude<DockTab, "">; label: string }[] = [
    { id: "generate", label: "AI STUDIO" },
    { id: "hum", label: "🎤 HUM" },
    { id: "library", label: "LIBRARY" },
    { id: "instruments", label: "INSTRUMENTS" },
    { id: "plugins", label: "PLUGINS" },
    { id: "mcp", label: "MCP" },
  ];
</script>

{#if ui.dock}
  <aside class="dock glass" aria-label="Side panel" style:width="{ui.dockWidth}px">
    <PanelResizeHandle
      axis="x"
      size={ui.dockWidth}
      spec={DOCK_RESIZE}
      label="Resize side panel"
      onresize={(px) => (ui.dockWidth = px)}
    />
    <div class="tabs" role="tablist">
      {#each TABS as t (t.id)}
        <button
          class="tab mono"
          class:on={ui.dock === t.id}
          role="tab"
          aria-selected={ui.dock === t.id}
          onclick={() => (ui.dock = t.id)}
        >
          {t.label}
          {#if t.id === "mcp" && mcp.pending.length > 0}
            <span class="alertdot"></span>
          {/if}
        </button>
      {/each}
      <button class="closer mono" title="Close panel" onclick={() => (ui.dock = "")}>×</button>
    </div>

    <div class="content">
      {#if ui.dock === "generate"}
        <AiStudio />
      {:else if ui.dock === "hum"}
        <HumPanel />
      {:else if ui.dock === "library"}
        <LibraryPanel />
      {:else if ui.dock === "instruments"}
        <InstrumentBrowser />
      {:else if ui.dock === "plugins"}
        {#if zyn.openInstanceId}
          <ZynPatchBrowser />
        {:else if plugins.openInstanceId}
          <PluginParamPanel />
        {:else}
          <PluginBrowser />
        {/if}
      {:else if ui.dock === "mcp"}
        <McpPanel />
      {/if}
    </div>
  </aside>
{/if}

<style>
  .dock {
    position: absolute;
    top: 0;
    right: 0;
    bottom: 0;
    width: 340px;
    z-index: 30;
    display: flex;
    flex-direction: column;
    border-top: none;
    border-bottom: none;
    border-right: none;
    background: rgba(8, 10, 19, 0.88);
    animation: dock-in 160ms ease-out;
  }
  @keyframes dock-in {
    from {
      transform: translateX(24px);
      opacity: 0;
    }
  }

  .tabs {
    flex: none;
    display: flex;
    align-items: center;
    gap: 2px;
    padding: 8px 10px 0;
    border-bottom: 1px solid var(--glass-border);
  }
  .tab {
    position: relative;
    font-size: 9px;
    letter-spacing: 0.14em;
    padding: 7px 7px 9px;
    white-space: nowrap;
    border: none;
    background: transparent;
    color: var(--text-faint);
    cursor: pointer;
    border-bottom: 2px solid transparent;
  }
  .tab.on {
    color: var(--cyan);
    border-bottom-color: var(--cyan);
    text-shadow: 0 0 8px var(--cyan-dim);
  }
  .alertdot {
    position: absolute;
    top: 4px;
    right: 2px;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--amber);
    box-shadow: 0 0 6px rgba(255, 200, 87, 0.8);
    animation: dot-pulse 1s ease-in-out infinite;
  }
  @keyframes dot-pulse {
    50% {
      opacity: 0.5;
    }
  }
  .closer {
    margin-left: auto;
    border: none;
    background: transparent;
    color: var(--text-faint);
    font-size: 14px;
    cursor: pointer;
    padding: 2px 6px;
  }
  .closer:hover {
    color: var(--red);
  }

  .content {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    padding: 12px;
  }
</style>
