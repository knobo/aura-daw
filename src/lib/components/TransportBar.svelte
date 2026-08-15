<script lang="ts">
  /**
   * Top transport bar: wordmark, transport buttons, rAF-driven timecode,
   * tempo block, snap/follow toggles and engine/renderer badges.
   *
   * The right-hand chip cluster is adaptive: a hidden copy of the row is
   * measured, and chips that don't fit the window collapse into a ⋯ menu,
   * lowest-priority first (see utils/toolbar-overflow).
   */
  import { backend } from "../tauri";
  import { transport } from "../state/transport.svelte";
  import { project } from "../state/project.svelte";
  import { view } from "../state/view.svelte";
  import { resetUiZoom, ui, toggleDock, zoomUiIn, zoomUiOut } from "../state/ui.svelte";
  import { prefs } from "../prefs/prefs.svelte";
  import { mcp } from "../state/mcp.svelte";
  import { exporter } from "../state/exporter.svelte";
  import { hum } from "../state/hum.svelte";
  import { toasts } from "../state/toasts.svelte";
  import { formatBarsBeats, formatClock } from "../utils/format";
  import { computeVisible } from "../utils/toolbar-overflow";
  import ProjectMenu from "./project/ProjectMenu.svelte";

  let clockEl: HTMLSpanElement | undefined = $state();
  let barsEl: HTMLSpanElement | undefined = $state();

  const anyArmed = $derived(project.tracks.some((t) => t.armed));

  $effect(() => {
    const on = prefs.values.metronome;
    const gain = prefs.values.metronomeGain;
    const bars = Number(prefs.values.countInBars);
    void backend.setMetronome?.(on, gain, Number.isFinite(bars) ? bars : 0);
  });

  // rAF timecode — writes textContent directly, outside reactivity
  $effect(() => {
    let raf = 0;
    const tick = (now: number) => {
      raf = requestAnimationFrame(tick);
      if (!clockEl || !barsEl) return;
      const pos = transport.positionAt(now);
      clockEl.textContent = formatClock(pos, transport.snap.sampleRate);
      barsEl.textContent = formatBarsBeats(
        pos,
        transport.snap.sampleRate,
        transport.snap.tempoBpm,
        project.timeSignature[0],
      );
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  });

  async function toggleRecord() {
    if (transport.isRecording || (transport.snap.countInLeftSamples ?? 0) > 0) {
      try {
        await backend.stopRecording();
      } catch (err) {
        // An Err means the take WAS registered (one undo entry, MIDI
        // included) and only the WAV write failed — never a retry signal,
        // and never a reason to skip the stop below.
        toasts.error("TAKE AUDIO NOT WRITTEN", String(err));
      }
      await transport.pause(); // halt, keep the playhead at the take end
    } else {
      await backend.startRecording(null);
      await transport.init(); // refresh snapshot (state -> recording)
    }
  }

  // ---- adaptive right cluster ------------------------------------------

  type RightItem = {
    id: string;
    kind: "chip" | "divider" | "badge";
    /** Higher survives longer when space runs out. */
    priority: number;
    label?: string;
    title?: string;
    cls?: string;
    on?: boolean;
    busy?: boolean;
    alert?: boolean;
    led?: "ok" | "pending" | null;
    onClick?: () => void;
  };

  const RIGHT_GAP = 8; // keep in sync with .right / .measure gap below

  const items: RightItem[] = $derived([
    {
      id: "studio",
      kind: "chip",
      priority: 13,
      cls: "studio",
      label: "✦ AI STUDIO",
      title: "AI Studio — prompt-driven generation (g)",
      on: ui.dock === "generate",
      onClick: () => toggleDock("generate"),
    },
    {
      id: "hum",
      kind: "chip",
      priority: 10,
      cls: "hum",
      label: "🎤 HUM",
      title: "Hum a melody — sing/hum into a mic, get a MIDI melody clip",
      on: ui.dock === "hum",
      busy: hum.busy,
      onClick: () => toggleDock("hum"),
    },
    {
      id: "lib",
      kind: "chip",
      priority: 5,
      label: "LIB",
      title: "Library — samples, project clips, presets",
      on: ui.dock === "library",
      onClick: () => toggleDock("library"),
    },
    {
      id: "inst",
      kind: "chip",
      priority: 7,
      label: "INST",
      title: "Sampler instrument browser",
      on: ui.dock === "instruments",
      onClick: () => toggleDock("instruments"),
    },
    {
      id: "plug",
      kind: "chip",
      priority: 6,
      label: "PLUG",
      title: "Plugin browser — CLAP / LV2 instruments",
      on: ui.dock === "plugins",
      onClick: () => toggleDock("plugins"),
    },
    {
      id: "mcp",
      kind: "chip",
      priority: 12,
      cls: "mcpchip",
      label: "MCP",
      title: `MCP agent control${mcp.pending.length ? ` — ${mcp.pending.length} awaiting confirmation` : ""}`,
      on: ui.dock === "mcp",
      alert: mcp.pending.length > 0,
      led: mcp.status?.running ? (mcp.pending.length > 0 ? "pending" : "ok") : null,
      onClick: () => toggleDock("mcp"),
    },
    { id: "d1", kind: "divider", priority: 0 },
    {
      id: "snap",
      kind: "chip",
      priority: 5,
      label: "SNAP",
      title: "Snap clip drags to the grid",
      on: view.snap,
      onClick: () => (view.snap = !view.snap),
    },
    {
      id: "follow",
      kind: "chip",
      priority: 4,
      label: "FOLLOW",
      title: "Follow the playhead",
      on: view.follow,
      onClick: () => (view.follow = !view.follow),
    },
    {
      id: "click",
      kind: "chip",
      priority: 8,
      cls: "clickchip",
      label: "CLICK",
      title: "Metronome — click on every beat while playing or recording",
      on: prefs.values.metronome,
      onClick: () => prefs.set("metronome", !prefs.values.metronome),
    },
    {
      id: "countin",
      kind: "chip",
      priority: 8,
      label: prefs.values.countInBars === "0" ? "COUNT" : `COUNT ${prefs.values.countInBars}`,
      title: "Count-in bars before record (off / 1 / 2 / 4)",
      on: prefs.values.countInBars !== "0",
      onClick: () => {
        const order = ["0", "1", "2", "4"] as const;
        const i = order.indexOf(prefs.values.countInBars);
        prefs.set("countInBars", order[(i + 1) % order.length]);
      },
    },
    {
      id: "loop",
      kind: "chip",
      priority: 9,
      cls: "loopchip",
      label: "LOOP",
      title: "Loop the region between the loop pins (drag the top strip of the ruler to set it)",
      on: transport.snap.loopEnabled,
      onClick: () => transport.toggleLoop(0, 4 * project.samplesPerBar),
    },
    {
      id: "stopend",
      kind: "chip",
      priority: 3,
      label: "STOP@END",
      title: "Stop when the playhead reaches the end of the material (ignored while looping or recording)",
      on: transport.snap.stopAtEnd,
      onClick: () => transport.setStopAtEnd(!transport.snap.stopAtEnd),
    },
    {
      id: "flash",
      kind: "chip",
      priority: 2,
      label: "FLASH",
      title: "Flash notes at the moment they play",
      on: prefs.values.noteFlash,
      onClick: () => prefs.set("noteFlash", !prefs.values.noteFlash),
    },
    {
      id: "export",
      kind: "chip",
      priority: 11,
      cls: "exportchip",
      label: "⤓ EXPORT",
      title: "Export — offline bounce to wav/mp3/flac/ogg/m4a",
      on: exporter.open,
      busy: exporter.busy,
      onClick: () => exporter.openDialog(),
    },
    { id: "d2", kind: "divider", priority: 0 },
    {
      id: "zoomout",
      kind: "chip",
      priority: 1,
      label: "A−",
      title: "Interface zoom out — smaller UI (Ctrl/Cmd −)",
      onClick: zoomUiOut,
    },
    {
      id: "zoomreset",
      kind: "chip",
      priority: 1,
      label: `${Math.round(prefs.values.uiZoom * 100)}%`,
      title: "Reset interface zoom (Ctrl/Cmd 0)",
      onClick: resetUiZoom,
    },
    {
      id: "zoomin",
      kind: "chip",
      priority: 1,
      label: "A+",
      title: "Interface zoom in — bigger UI (Ctrl/Cmd +)",
      onClick: zoomUiIn,
    },
    {
      id: "renderer",
      kind: "badge",
      priority: 0,
      label: ui.rendererKind || "· · ·",
      title: "Waveform renderer in use",
    },
    {
      id: "mode",
      kind: "badge",
      priority: 0,
      cls: "mode",
      label: backend.mode === "demo" ? "DEMO" : "ENGINE",
      title: backend.mode === "demo" ? "Engine offline — synthetic data" : "Rust engine connected",
    },
  ]);

  let rightEl: HTMLDivElement | undefined = $state();
  let measureEl: HTMLDivElement | undefined = $state();
  // Start with everything collapsed; the first measurement fills the row
  // before paint settles, so there is no flash of an overflowing bar.
  let visibleIds: Set<string> = $state(new Set());
  let overflowIds: string[] = $state([]);
  let moreOpen = $state(false);

  const overflowItems = $derived(
    overflowIds
      .map((id) => items.find((it) => it.id === id))
      .filter((it): it is RightItem => it !== undefined),
  );

  function recompute() {
    if (!rightEl || !measureEl) return;
    const widths = new Map<string, number>();
    for (const child of measureEl.children) {
      const id = child.getAttribute("data-mid");
      if (id) widths.set(id, (child as HTMLElement).offsetWidth);
    }
    const result = computeVisible(
      items.map((it) => ({
        id: it.id,
        width: widths.get(it.id) ?? 0,
        priority: it.priority,
        divider: it.kind === "divider",
      })),
      rightEl.clientWidth,
      widths.get("__more") ?? 0,
      RIGHT_GAP,
    );
    visibleIds = result.visible;
    overflowIds = result.overflow;
    if (result.overflow.length === 0) moreOpen = false;
  }

  $effect(() => {
    // Reading `items` here re-measures when any label / state changes;
    // the observers catch window resizes and measure-row width changes.
    void items;
    const ro = new ResizeObserver(() => recompute());
    if (rightEl) ro.observe(rightEl);
    if (measureEl) ro.observe(measureEl);
    recompute();
    return () => ro.disconnect();
  });

  function runFromMenu(it: RightItem) {
    moreOpen = false;
    it.onClick?.();
  }
</script>

{#snippet rowItem(it: RightItem)}
  {#if it.kind === "divider"}
    <span class="divider" data-mid={it.id}></span>
  {:else if it.kind === "badge"}
    <span class="badge mono {it.cls ?? ''}" data-mid={it.id} title={it.title}>{it.label}</span>
  {:else}
    <button
      class="chip {it.cls ?? ''}"
      class:on={it.on}
      class:busychip={it.busy}
      class:alert={it.alert}
      data-mid={it.id}
      title={it.title}
      onclick={it.onClick}
    >
      {it.label}{#if it.led}<span class="mcpled" class:pending={it.led === "pending"}></span>{/if}
    </button>
  {/if}
{/snippet}

<header class="bar glass">
  <div class="left">
    <div class="wordmark mono">AURA</div>
    <ProjectMenu />
  </div>

  <div class="center">
    <div class="tbuttons">
      <button
        class="tbtn"
        title="Jump to start (home) — keeps playing"
        aria-label="Jump to start"
        onclick={() => transport.seek(0)}
      >
        <svg viewBox="0 0 14 14" width="12" height="12">
          <rect x="3" y="2.5" width="1.8" height="9" fill="currentColor" />
          <path d="M11.5 2.5v9L5.6 7z" fill="currentColor" />
        </svg>
      </button>
      <button
        class="tbtn"
        title="Stop and return to start"
        aria-label="Stop"
        onclick={() => transport.stop()}
      >
        <svg viewBox="0 0 14 14" width="12" height="12"><rect x="3" y="3" width="8" height="8" fill="currentColor" /></svg>
      </button>
      <button
        class="tbtn play"
        class:active={transport.isPlaying}
        title={transport.isPlaying ? "Pause (space)" : "Play (space)"}
        aria-label={transport.isPlaying ? "Pause" : "Play"}
        onclick={() => transport.togglePlay()}
      >
        {#if transport.isPlaying}
          <svg viewBox="0 0 14 14" width="12" height="12"><path d="M3.5 2.5h2.6v9H3.5zM7.9 2.5h2.6v9H7.9z" fill="currentColor" /></svg>
        {:else}
          <svg viewBox="0 0 14 14" width="12" height="12"><path d="M4 2.5v9l7.5-4.5z" fill="currentColor" /></svg>
        {/if}
      </button>
      <button
        class="tbtn rec"
        class:recording={transport.isRecording}
        title={
          (transport.snap.countInLeftSamples ?? 0) > 0
            ? "Counting in — click to cancel"
            : anyArmed
              ? "Record armed tracks"
              : "Arm a track first"
        }
        aria-label="Record"
        class:counting={(transport.snap.countInLeftSamples ?? 0) > 0}
        disabled={!anyArmed && !transport.isRecording && !(transport.snap.countInLeftSamples ?? 0)}
        onclick={toggleRecord}
      >
        <svg viewBox="0 0 14 14" width="12" height="12"><circle cx="7" cy="7" r="4.2" fill="currentColor" /></svg>
      </button>
    </div>

    <div class="timecode mono" class:live={transport.isPlaying}>
      <span class="clock" bind:this={clockEl}>00:00.000</span>
      <span class="bars" bind:this={barsEl}>001.1.1</span>
    </div>

    <div class="tempo mono">
      <span class="silk">tempo</span>
      <span class="val">{project.tempoBpm.toFixed(1)}</span>
      <span class="unit silk">bpm · {project.timeSignature[0]}/{project.timeSignature[1]}</span>
    </div>
  </div>

  <div class="right" bind:this={rightEl}>
    <!-- hidden twin of the full row; only ever measured, never seen -->
    <div class="measure" bind:this={measureEl} aria-hidden="true">
      {#each items as it (it.id)}
        {@render rowItem(it)}
      {/each}
      <button class="chip morebtn" data-mid="__more" tabindex="-1">⋯</button>
    </div>

    {#each items as it (it.id)}
      {#if visibleIds.has(it.id)}
        {@render rowItem(it)}
      {/if}
    {/each}

    {#if overflowIds.length > 0}
      <div class="morewrap">
        <button
          class="chip morebtn"
          class:on={moreOpen}
          class:alert={!moreOpen && overflowItems.some((it) => it.alert)}
          title="More controls"
          aria-label="More controls"
          aria-haspopup="menu"
          aria-expanded={moreOpen}
          onclick={() => (moreOpen = !moreOpen)}>⋯</button
        >
        {#if moreOpen}
          <div class="backdrop" role="presentation" onpointerdown={() => (moreOpen = false)}></div>
          <div class="menu glass mono" role="menu" aria-label="More controls">
            {#each overflowItems as it (it.id)}
              {#if it.kind === "badge"}
                <div class="mbadge {it.cls ?? ''}" title={it.title}>{it.label}</div>
              {:else}
                <button
                  role="menuitem"
                  class="mrow"
                  class:on={it.on}
                  class:alert={it.alert}
                  title={it.title}
                  onclick={() => runFromMenu(it)}
                >
                  <span>{it.label}</span>
                  {#if it.led}
                    <span class="mcpled" class:pending={it.led === "pending"}></span>
                  {:else if it.on}
                    <span class="mdot">●</span>
                  {/if}
                </button>
              {/if}
            {/each}
          </div>
        {/if}
      </div>
    {/if}
  </div>
</header>

<style>
  .bar {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 16px;
    height: 56px;
    padding: 0 16px;
    border-left: none;
    border-right: none;
    border-top: none;
    position: relative;
    /* Above Dock (30) and PanelResizeHandle (40): `z-index` here opens a
       stacking context, so the ⋯ menu's own z-index only sorts INSIDE the
       bar — this value is what the whole bar, dropdown included, competes
       with. Below Toasts (90), which must stay on top of everything. */
    z-index: 50;
  }

  .left {
    display: flex;
    align-items: center;
    gap: 14px;
    min-width: var(--rail-width);
    flex-shrink: 0;
  }

  .wordmark {
    font-size: 17px;
    font-weight: 600;
    letter-spacing: 0.55em;
    padding-left: 0.25em;
    color: var(--text);
    text-shadow:
      0 0 14px var(--cyan-dim),
      1px 0 0 rgba(255, 79, 216, 0.55),
      -1px 0 0 rgba(82, 229, 255, 0.55);
  }

  .center {
    display: flex;
    align-items: center;
    gap: 20px;
    flex-shrink: 0;
  }

  .tbuttons {
    display: flex;
    gap: 6px;
  }
  .tbtn {
    width: 34px;
    height: 30px;
    display: grid;
    place-items: center;
    border-radius: 5px;
    border: 1px solid var(--glass-border);
    background: rgba(16, 20, 42, 0.5);
    color: var(--text-dim);
    cursor: pointer;
    transition: color 120ms, border-color 120ms, box-shadow 120ms;
  }
  .tbtn:hover {
    color: var(--text);
    border-color: rgba(122, 160, 220, 0.3);
  }
  .tbtn.play.active {
    color: var(--cyan);
    border-color: var(--cyan-dim);
    box-shadow: 0 0 12px rgba(82, 229, 255, 0.25);
  }
  .tbtn.rec {
    color: rgba(255, 65, 82, 0.6);
  }
  .tbtn.rec:disabled {
    opacity: 0.35;
    cursor: not-allowed;
  }
  .tbtn.rec.counting {
    color: var(--accent);
    border-color: var(--accent);
    animation: countpulse 0.6s ease-in-out infinite alternate;
  }
  @keyframes countpulse {
    from { box-shadow: 0 0 0 0 rgba(92, 242, 184, 0.0); }
    to { box-shadow: 0 0 0 4px rgba(92, 242, 184, 0.25); }
  }
  .tbtn.rec.recording {
    color: var(--red);
    border-color: rgba(255, 65, 82, 0.5);
    box-shadow: 0 0 14px rgba(255, 65, 82, 0.35);
    animation: rec-pulse 1.2s ease-in-out infinite;
  }
  @keyframes rec-pulse {
    50% {
      box-shadow: 0 0 4px rgba(255, 65, 82, 0.15);
    }
  }

  .timecode {
    display: flex;
    align-items: baseline;
    gap: 12px;
    padding: 4px 14px;
    border-radius: 6px;
    border: 1px solid var(--glass-border);
    background: rgba(5, 7, 13, 0.75);
  }
  .clock {
    font-size: 21px;
    font-weight: 500;
    letter-spacing: 0.04em;
    color: var(--text);
  }
  .timecode.live .clock {
    color: var(--cyan);
    text-shadow: 0 0 10px var(--cyan-dim);
  }
  .bars {
    font-size: 12px;
    color: var(--text-dim);
  }

  .tempo {
    display: flex;
    flex-direction: column;
    line-height: 1.25;
  }
  .tempo .val {
    font-size: 15px;
    color: var(--text);
  }

  .right {
    display: flex;
    align-items: center;
    gap: 8px; /* RIGHT_GAP */
    justify-content: flex-end;
    flex: 1 1 auto;
    min-width: 0;
    position: relative;
  }

  .measure {
    position: absolute;
    top: -9999px;
    left: 0;
    display: flex;
    align-items: center;
    gap: 8px; /* RIGHT_GAP */
    visibility: hidden;
    pointer-events: none;
  }

  .chip {
    font-family: var(--font-mono);
    font-size: 9px;
    letter-spacing: 0.18em;
    padding: 4px 8px;
    border-radius: 4px;
    border: 1px solid var(--glass-border);
    background: transparent;
    color: var(--text-faint);
    cursor: pointer;
    white-space: nowrap;
  }
  .chip.on {
    color: var(--cyan);
    border-color: var(--cyan-dim);
  }
  .chip.loopchip.on {
    color: var(--amber);
    border-color: rgba(255, 200, 87, 0.5);
    box-shadow: 0 0 10px rgba(255, 200, 87, 0.2);
  }
  .chip.studio {
    background: linear-gradient(
      100deg,
      rgba(82, 229, 255, 0.08),
      rgba(255, 79, 216, 0.08)
    );
  }
  .chip.studio:hover,
  .chip.studio.on {
    color: var(--cyan);
    border-color: var(--cyan-dim);
    box-shadow: 0 0 10px rgba(82, 229, 255, 0.2);
  }
  .chip.hum {
    background: linear-gradient(
      100deg,
      rgba(255, 79, 216, 0.08),
      rgba(157, 123, 255, 0.08)
    );
  }
  .chip.hum:hover,
  .chip.hum.on {
    color: var(--magenta);
    border-color: var(--magenta-dim);
    box-shadow: 0 0 10px rgba(255, 79, 216, 0.2);
  }
  .chip.exportchip:hover,
  .chip.exportchip.on {
    color: #5cf2b8;
    border-color: rgba(92, 242, 184, 0.45);
    box-shadow: 0 0 10px rgba(92, 242, 184, 0.2);
  }
  .chip.busychip {
    animation: chip-busy 1s ease-in-out infinite;
  }
  @keyframes chip-busy {
    50% {
      opacity: 0.55;
    }
  }
  .mcpchip {
    display: inline-flex;
    align-items: center;
    gap: 5px;
  }
  .mcpchip.alert,
  .morebtn.alert {
    color: var(--amber);
    border-color: rgba(255, 200, 87, 0.5);
    box-shadow: 0 0 10px rgba(255, 200, 87, 0.25);
  }
  .mcpled {
    width: 5px;
    height: 5px;
    border-radius: 50%;
    background: #5cf2b8;
    box-shadow: 0 0 5px rgba(92, 242, 184, 0.8);
  }
  .mcpled.pending {
    background: var(--amber);
    box-shadow: 0 0 6px rgba(255, 200, 87, 0.9);
    animation: mcp-blink 0.9s ease-in-out infinite;
  }
  @keyframes mcp-blink {
    50% {
      opacity: 0.35;
    }
  }
  .divider {
    width: 1px;
    height: 16px;
    background: var(--glass-border);
    margin: 0 2px;
    flex-shrink: 0;
  }

  .badge {
    font-size: 9px;
    letter-spacing: 0.18em;
    padding: 4px 8px;
    border-radius: 4px;
    color: var(--text-faint);
    border: 1px dashed var(--glass-border);
    white-space: nowrap;
  }
  .badge.mode {
    color: var(--magenta);
    border-color: var(--magenta-dim);
  }

  /* ---- overflow ⋯ menu ---- */
  .morewrap {
    position: relative;
    display: flex;
  }
  .morebtn {
    min-width: 26px;
    font-size: 11px;
    letter-spacing: 0;
    line-height: 1;
  }
  .morebtn.on,
  .morebtn:hover {
    color: var(--cyan);
    border-color: var(--cyan-dim);
  }
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 89;
  }
  .menu {
    position: absolute;
    top: calc(100% + 10px);
    right: 0;
    z-index: 90;
    min-width: 170px;
    display: flex;
    flex-direction: column;
    padding: 5px;
    border-radius: 7px;
    background: rgba(8, 10, 19, 0.96);
    max-height: calc(100vh - 90px);
    overflow-y: auto;
  }
  .mrow {
    display: flex;
    justify-content: space-between;
    align-items: center;
    gap: 14px;
    background: transparent;
    border: none;
    border-radius: 4px;
    padding: 7px 9px;
    font-size: 10px;
    letter-spacing: 0.16em;
    color: var(--text-dim);
    cursor: pointer;
    white-space: nowrap;
  }
  .mrow:hover {
    background: rgba(82, 229, 255, 0.09);
    color: var(--cyan);
  }
  .mrow.on {
    color: var(--cyan);
  }
  .mrow.alert {
    color: var(--amber);
  }
  .mdot {
    font-size: 8px;
    color: var(--cyan);
  }
  .mbadge {
    padding: 6px 9px 4px;
    border-top: 1px solid var(--glass-border);
    margin-top: 4px;
    font-size: 9px;
    letter-spacing: 0.18em;
    color: var(--text-faint);
    white-space: nowrap;
  }
  .mbadge.mode {
    color: var(--magenta);
  }
</style>
