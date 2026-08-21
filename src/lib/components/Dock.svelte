<script lang="ts">
  /**
   * Right dock: slide-in drawer hosting AI Studio, the instrument browser
   * and the MCP control panel. Toggled from the transport bar chips.
   *
   * It sits in one of two layouts, chosen by the `dockSide` preference. As
   * an OVERLAY it floats above the timeline and casts leftward onto it — the
   * original behaviour, and the right one for a translucent theme, where you
   * can see what you are covering. DOCKED, it is a static flex item and the
   * timeline gets the remaining width, which is what an opaque panel wants:
   * a wall you cannot see through should not be hiding half the arrangement.
   * The layout switch itself lives in App.svelte, which owns the row.
   */
  import { DOCK_SHORTCUT, ui, type DockTab } from "../state/ui.svelte";
  import { prefs } from "../prefs/prefs.svelte";
  import { DOCK_RESIZE } from "../utils/panel-resize";
  import PanelResizeHandle from "./PanelResizeHandle.svelte";
  import { mcp } from "../state/mcp.svelte";
  import { plugins } from "../state/plugins.svelte";
  import { zyn } from "../state/zynpatches.svelte";
  import AiStudio from "./generate/AiStudio.svelte";
  import ComposerPanel from "./composer/ComposerPanel.svelte";
  import HumPanel from "./hum/HumPanel.svelte";
  import LibraryPanel from "./library/LibraryPanel.svelte";
  import InstrumentBrowser from "./instruments/InstrumentBrowser.svelte";
  import PluginBrowser from "./plugins/PluginBrowser.svelte";
  import PluginParamPanel from "./plugins/PluginParamPanel.svelte";
  import ZynPatchBrowser from "./plugins/ZynPatchBrowser.svelte";
  import McpPanel from "./mcp/McpPanel.svelte";
  import MidiPanel from "./midi/MidiPanel.svelte";
  import HistoryPanel from "./history/HistoryPanel.svelte";
  import { historyBrowser } from "../state/history.svelte";

  const docked = $derived(prefs.values.dockSide === "docked");

  function selectTab(tab: Exclude<DockTab, "">) {
    ui.dock = tab;
    if (tab === "history") void historyBrowser.load();
  }

  const TABS: { id: Exclude<DockTab, "">; label: string }[] = [
    { id: "composer", label: "♪ COMPOSER" },
    { id: "generate", label: "AI STUDIO" },
    { id: "hum", label: "🎤 HUM" },
    { id: "library", label: "LIBRARY" },
    { id: "instruments", label: "INSTRUMENTS" },
    { id: "plugins", label: "PLUGINS" },
    { id: "mcp", label: "MCP" },
    { id: "midi", label: "⇄ MIDI" },
    { id: "history", label: "HISTORY" },
  ];
</script>

{#if ui.dock}
  <aside class="dock glass" class:docked aria-label="Side panel" style:width="{ui.dockWidth}px">
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
          title="{t.label} — press {DOCK_SHORTCUT[t.id].toUpperCase()}"
          onclick={() => selectTab(t.id)}
        >
          {t.label}
          <span class="key silk" aria-hidden="true">{DOCK_SHORTCUT[t.id]}</span>
          {#if t.id === "mcp" && mcp.pending.length > 0}
            <span class="alertdot"></span>
          {/if}
        </button>
      {/each}
      <button class="closer mono" title="Close panel" onclick={() => (ui.dock = "")}>×</button>
    </div>

    <div class="content">
      {#if ui.dock === "composer"}
        <ComposerPanel />
      {:else if ui.dock === "generate"}
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
      {:else if ui.dock === "midi"}
        <MidiPanel />
      {:else if ui.dock === "history"}
        <HistoryPanel />
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
    /* Fill and blur are the theme's call (`panelAlpha`), not a hard-coded
       0.88. `--panel-blur` is DERIVED from that alpha, so an opaque panel
       cannot also be paying for a backdrop-filter over the largest
       always-on surface in the app. `.glass` still sets its own blur, so
       this overrides it rather than relying on order. */
    background: var(--panel);
    backdrop-filter: blur(var(--panel-blur)) saturate(1.2);
    -webkit-backdrop-filter: blur(var(--panel-blur)) saturate(1.2);
    /* Floating: the dock hangs OVER the timeline, so it casts leftward onto
       what it covers. */
    box-shadow: calc(-6px * var(--relief)) 0 calc(20px * var(--relief))
      rgb(var(--shadow-rgb) / calc(var(--relief) * 0.45));
    animation: dock-in 160ms ease-out;
  }
  /* Docked: a column in the row, so `position` and the overlay's cast
     shadow both come off. It keeps a hard left edge instead — the seam
     between two panels that meet, rather than one floating above another. */
  .dock.docked {
    position: relative;
    inset: auto;
    flex: none;
    box-shadow: none;
    border-left: var(--border-width) solid var(--glass-border);
    animation: none;
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
    /* Eight tabs no longer fit one row at the default dock width, and a row
       that overflows silently hides a whole panel from anyone who does not
       know the keyboard shortcut. */
    flex-wrap: wrap;
    align-items: center;
    gap: 2px;
    padding: 8px 10px 0;
    border-bottom: 1px solid var(--glass-border);
  }
  .tab {
    position: relative;
    font-size: 9px;
    letter-spacing: 0.14em;
    /* Tight horizontally so eight tabs fit in as few rows as the dock's
       current width allows. */
    padding: 7px 5px 9px;
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
    text-shadow: 0 0 calc(8px * var(--glow-scale)) var(--cyan-dim);
    /* The active tab is the key that is pressed IN: it sits in the panel
       rather than on it, which is what makes the row read as hardware. */
    border-radius: var(--ctrl-radius) var(--ctrl-radius) 0 0;
    background-color: rgb(var(--bg-2-rgb) / 0.55);
    box-shadow: var(--bevel-inset);
  }
  .key {
    margin-left: 4px;
    padding: 0 3px;
    border-radius: 2px;
    border: 1px solid var(--glass-border);
    text-transform: uppercase;
  }
  .alertdot {
    position: absolute;
    top: 4px;
    right: 2px;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--amber);
    box-shadow: 0 0 calc(6px * var(--glow-scale)) rgb(var(--amber-rgb) / 0.8);
    animation: dot-pulse 1s ease-in-out infinite;
  }
  @keyframes dot-pulse {
    50% {
      opacity: 0.5;
    }
  }
  .closer {
    margin-left: auto;
    align-self: flex-start;
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
    padding: 8px;
  }
</style>
