<script lang="ts">
  /**
   * Top transport bar: wordmark, transport buttons, rAF-driven timecode,
   * tempo block, snap/follow toggles and engine/renderer badges.
   */
  import { backend } from "../tauri";
  import { transport } from "../state/transport.svelte";
  import { project } from "../state/project.svelte";
  import { view } from "../state/view.svelte";
  import { ui, toggleDock } from "../state/ui.svelte";
  import { mcp } from "../state/mcp.svelte";
  import { exporter } from "../state/exporter.svelte";
  import { hum } from "../state/hum.svelte";
  import { formatBarsBeats, formatClock } from "../utils/format";

  let clockEl: HTMLSpanElement | undefined = $state();
  let barsEl: HTMLSpanElement | undefined = $state();

  const anyArmed = $derived(project.tracks.some((t) => t.armed));

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
    if (transport.isRecording) {
      await backend.stopRecording();
      await transport.pause(); // halt, keep the playhead at the take end
    } else {
      await backend.startRecording(null);
      await transport.init(); // refresh snapshot (state -> recording)
    }
  }
</script>

<header class="bar glass">
  <div class="left">
    <div class="wordmark mono">AURA</div>
    <div class="project">
      <span class="silk">project</span>
      <span class="pname">{project.name}</span>
    </div>
  </div>

  <div class="center">
    <div class="tbuttons">
      <button
        class="tbtn"
        title="Stop (return)"
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
        title={anyArmed ? "Record armed tracks" : "Arm a track first"}
        aria-label="Record"
        disabled={!anyArmed && !transport.isRecording}
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

  <div class="right">
    <button
      class="chip studio"
      class:on={ui.dock === "generate"}
      title="AI Studio — prompt-driven generation (g)"
      onclick={() => toggleDock("generate")}>✦ AI STUDIO</button
    >
    <button
      class="chip hum"
      class:on={ui.dock === "hum"}
      class:busychip={hum.busy}
      title="Hum a melody — sing/hum into a mic, get a MIDI melody clip"
      onclick={() => toggleDock("hum")}>🎤 HUM</button
    >
    <button
      class="chip"
      class:on={ui.dock === "instruments"}
      title="Sampler instrument browser"
      onclick={() => toggleDock("instruments")}>INST</button
    >
    <button
      class="chip"
      class:on={ui.dock === "plugins"}
      title="Plugin browser — CLAP / LV2 instruments"
      onclick={() => toggleDock("plugins")}>PLUG</button
    >
    <button
      class="chip mcpchip"
      class:on={ui.dock === "mcp"}
      class:alert={mcp.pending.length > 0}
      title="MCP agent control{mcp.pending.length ? ` — ${mcp.pending.length} awaiting confirmation` : ''}"
      onclick={() => toggleDock("mcp")}
    >
      MCP
      {#if mcp.status?.running}<span class="mcpled" class:pending={mcp.pending.length > 0}></span>{/if}
    </button>
    <span class="divider"></span>
    <button
      class="chip"
      class:on={view.snap}
      title="Snap clip drags to the grid"
      onclick={() => (view.snap = !view.snap)}>SNAP</button
    >
    <button
      class="chip"
      class:on={view.follow}
      title="Follow the playhead"
      onclick={() => (view.follow = !view.follow)}>FOLLOW</button
    >
    <button
      class="chip loopchip"
      class:on={transport.snap.loopEnabled}
      title="Loop the region between the loop pins (drag the top strip of the ruler to set it)"
      onclick={() => transport.toggleLoop(0, 4 * project.samplesPerBar)}>LOOP</button
    >
    <button
      class="chip"
      class:on={ui.noteFlash}
      title="Flash notes at the moment they play"
      onclick={() => (ui.noteFlash = !ui.noteFlash)}>FLASH</button
    >
    <button
      class="chip exportchip"
      class:on={exporter.open}
      class:busychip={exporter.busy}
      title="Export — offline bounce to wav/mp3/flac/ogg/m4a"
      onclick={() => exporter.openDialog()}>⤓ EXPORT</button
    >
    <span class="badge mono" title="Waveform renderer in use">{ui.rendererKind || "· · ·"}</span>
    <span class="badge mode mono" title={backend.mode === "demo" ? "Engine offline — synthetic data" : "Rust engine connected"}>
      {backend.mode === "demo" ? "DEMO" : "ENGINE"}
    </span>
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
    z-index: 20;
  }

  .left {
    display: flex;
    align-items: center;
    gap: 14px;
    min-width: var(--rail-width);
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

  .project {
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .pname {
    font-size: 12px;
    color: var(--text-dim);
  }

  .center {
    display: flex;
    align-items: center;
    gap: 20px;
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
    gap: 8px;
    justify-content: flex-end;
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
  .mcpchip.alert {
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
  }

  .badge {
    font-size: 9px;
    letter-spacing: 0.18em;
    padding: 4px 8px;
    border-radius: 4px;
    color: var(--text-faint);
    border: 1px dashed var(--glass-border);
  }
  .badge.mode {
    color: var(--magenta);
    border-color: var(--magenta-dim);
  }
</style>
