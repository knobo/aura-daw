<script lang="ts">
  /**
   * Left-rail track header: color stripe, name, arm/mute/solo, gain fader
   * with dB readout, pan, and a compact per-track VU meter.
   */
  import type { TrackState } from "../types/ipc";
  import { project } from "../state/project.svelte";
  import { instruments } from "../state/instruments.svelte";
  import { plugins } from "../state/plugins.svelte";
  import { zyn } from "../state/zynpatches.svelte";
  import { automation } from "../state/automation.svelte";
  import { openPluginParams } from "../state/plugin-panel";
  import { ui } from "../state/ui.svelte";
  import { formatDb } from "../utils/format";
  import Meter from "./Meter.svelte";

  let { track, index }: { track: TrackState; index: number } = $props();

  const gainPct = $derived(((track.gainDb + 60) / 72) * 100);
  const instrument = $derived(
    track.kind === "midi" ? instruments.byId(track.instrumentId) : undefined,
  );
  /** Plugin-instance binding ("plugin:<id>" refs) — rendered distinctly. */
  const pluginInst = $derived(
    track.kind === "midi" ? plugins.instanceForRef(track.instrumentId) : undefined,
  );
  /** The bank patch loaded into that instance, if any (Zyn). The patch is
   * what a user picks and re-picks, so it — not the constant host name —
   * is what the chip shows once one is loaded. */
  const patch = $derived(pluginInst ? zyn.loaded[pluginInst.id] : undefined);

  function openInstrumentPanel() {
    if (pluginInst) {
      void openPluginParams(pluginInst.id);
    } else {
      ui.dock = "instruments";
    }
  }

  function onGain(e: Event) {
    const v = parseFloat((e.currentTarget as HTMLInputElement).value);
    void project.setGain(track.id, v);
  }
  function onPan(e: Event) {
    const v = parseFloat((e.currentTarget as HTMLInputElement).value);
    void project.setPan(track.id, v);
  }

  /** Gesture boundaries (Plan E Task 14): explicit begin/end brackets a
   * fader/pan drag so the backend coalesces the per-`input`-event commits
   * into one history-bound batch instead of one per pointer move. `oninput`
   * itself (`onGain`/`onPan` above) is unchanged — the coalescing happens
   * backend-side, not by throttling these calls. */
  function onGainPointerDown() {
    project.beginGesture("gain drag");
  }
  function onPanPointerDown() {
    project.beginGesture("pan drag");
  }
  function onGestureEnd() {
    project.endGesture();
  }
</script>

<div class="header" style:--track-color={track.color}>
  <div class="stripe"></div>
  <div class="body">
    <div class="row top">
      <span class="idx mono">{String(index + 1).padStart(2, "0")}</span>
      <span class="name" title={track.name}>{track.name}</span>
      {#if track.kind === "midi"}
        <button
          class="instchip mono"
          class:bound={!!instrument}
          class:plugin={!!pluginInst}
          class:stub={pluginInst?.status === "stub"}
          class:crashed={pluginInst?.status === "crashed"}
          title={pluginInst
            ? `Plugin: ${pluginInst.name} (${pluginInst.format}, ${pluginInst.status})${
                patch ? ` — patch: ${patch.bank} / ${patch.name}` : ""
              } — open params`
            : instrument
              ? `Instrument: ${instrument.name} — open browser`
              : "MIDI track — built-in polysynth; assign an instrument"}
          onclick={openInstrumentPanel}
        >
          {#if pluginInst}
            ⚡ {patch?.name ?? pluginInst.name}
          {:else if instrument}
            ⌁ {instrument.name}
          {:else}
            ⌁ polysynth
          {/if}
        </button>
      {/if}
      <button
        class="del"
        title="Remove track"
        aria-label="Remove track {track.name}"
        onclick={() => project.removeTrack(track.id)}>×</button
      >
    </div>

    <div class="row mid">
      <div class="toggles">
        <button
          class="tog arm"
          class:on={track.armed}
          title="Record arm"
          onclick={() => project.toggleArm(track.id)}>R</button
        >
        <button
          class="tog mute"
          class:on={track.muted}
          title="Mute"
          onclick={() => project.toggleMute(track.id)}>M</button
        >
        <button
          class="tog solo"
          class:on={track.soloed}
          title="Solo"
          onclick={() => project.toggleSolo(track.id)}>S</button
        >
        <button
          class="tog auto"
          class:on={automation.isVisible(track.id)}
          title="Show gain automation lane"
          aria-pressed={automation.isVisible(track.id)}
          onclick={() => automation.toggleVisible(track.id)}>A</button
        >
      </div>

      <div class="fader-wrap">
        <input
          class="fader"
          type="range"
          min="-60"
          max="12"
          step="0.1"
          value={track.gainDb}
          style:--fader-pct="{gainPct}%"
          style:--fader-fill={track.color}
          title="Gain"
          aria-label="Gain for {track.name}"
          oninput={onGain}
          onpointerdown={onGainPointerDown}
          onpointerup={onGestureEnd}
          onpointercancel={onGestureEnd}
        />
        <span class="db mono">{formatDb(track.gainDb)}</span>
      </div>

      <div class="pan-wrap" title="Pan">
        <span class="silk">pan</span>
        <input
          class="fader pan"
          type="range"
          min="-1"
          max="1"
          step="0.01"
          value={track.pan}
          style:--fader-pct="{((track.pan + 1) / 2) * 100}%"
          style:--fader-fill="var(--violet)"
          aria-label="Pan for {track.name}"
          oninput={onPan}
          onpointerdown={onPanPointerDown}
          onpointerup={onGestureEnd}
          onpointercancel={onGestureEnd}
          ondblclick={() => project.setPan(track.id, 0)}
        />
      </div>
    </div>

    <div class="row meter-row">
      <Meter trackId={track.id} height={10} />
    </div>
  </div>
</div>

<style>
  .header {
    display: flex;
    height: var(--track-height);
    border-bottom: 1px solid rgba(96, 130, 190, 0.08);
    background: linear-gradient(to right, rgba(16, 20, 42, 0.35), rgba(10, 13, 23, 0.15));
  }
  .stripe {
    width: 3px;
    background: var(--track-color);
    opacity: 0.85;
    box-shadow: 0 0 8px color-mix(in srgb, var(--track-color) 60%, transparent);
  }
  .body {
    flex: 1;
    min-width: 0;
    padding: 8px 10px 6px 9px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  .row {
    display: flex;
    align-items: center;
    gap: 8px;
  }

  .idx {
    font-size: 9px;
    color: var(--text-faint);
  }
  .name {
    flex: 1;
    min-width: 0;
    font-size: 12px;
    font-weight: 500;
    letter-spacing: 0.02em;
    color: var(--text);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .instchip {
    max-width: 92px;
    font-size: 8px;
    letter-spacing: 0.08em;
    padding: 2px 6px;
    border-radius: 3px;
    border: 1px dashed rgba(122, 160, 220, 0.25);
    background: transparent;
    color: var(--text-faint);
    cursor: pointer;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .instchip.bound {
    color: #5cf2b8;
    border-style: solid;
    border-color: rgba(92, 242, 184, 0.35);
  }
  .instchip:hover {
    color: var(--cyan);
    border-color: var(--cyan-dim);
  }
  /* plugin-hosted instrument — distinct from sampler (solid violet chip) */
  .instchip.plugin {
    color: var(--violet);
    border-style: solid;
    border-color: rgba(157, 123, 255, 0.45);
    background: rgba(157, 123, 255, 0.09);
    box-shadow: 0 0 6px rgba(157, 123, 255, 0.18);
  }
  .instchip.plugin.stub {
    color: var(--amber);
    border-color: rgba(255, 200, 87, 0.4);
    background: rgba(255, 200, 87, 0.07);
    box-shadow: none;
  }
  .instchip.plugin.crashed {
    color: var(--red);
    border-color: rgba(255, 65, 82, 0.45);
    background: rgba(255, 65, 82, 0.08);
  }
  .instchip.plugin:hover {
    color: var(--cyan);
    border-color: var(--cyan-dim);
  }

  .del {
    width: 16px;
    height: 16px;
    line-height: 1;
    border: none;
    background: transparent;
    color: transparent;
    font-size: 13px;
    cursor: pointer;
    border-radius: 3px;
  }
  .header:hover .del {
    color: var(--text-faint);
  }
  .del:hover {
    color: var(--red);
  }

  .toggles {
    display: flex;
    gap: 3px;
  }
  .tog {
    width: 20px;
    height: 18px;
    font-family: var(--font-mono);
    font-size: 9px;
    border-radius: 3px;
    border: 1px solid rgba(122, 160, 220, 0.15);
    background: transparent;
    color: var(--text-faint);
    cursor: pointer;
  }
  .tog.arm.on {
    color: #fff;
    background: rgba(255, 65, 82, 0.8);
    border-color: var(--red);
    box-shadow: 0 0 8px rgba(255, 65, 82, 0.4);
  }
  .tog.mute.on {
    color: var(--bg-0);
    background: var(--amber);
    border-color: var(--amber);
  }
  .tog.solo.on {
    color: var(--bg-0);
    background: var(--cyan);
    border-color: var(--cyan);
    box-shadow: 0 0 8px rgba(82, 229, 255, 0.4);
  }
  .tog.auto.on {
    color: var(--bg-0);
    background: var(--magenta);
    border-color: var(--magenta);
    box-shadow: 0 0 8px rgba(255, 79, 216, 0.4);
  }

  .fader-wrap {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .fader {
    flex: 1;
    min-width: 0;
  }
  .db {
    font-size: 9px;
    width: 34px;
    text-align: right;
    color: var(--text-dim);
  }

  .pan-wrap {
    display: flex;
    align-items: center;
    gap: 4px;
    width: 64px;
  }
  .pan-wrap .pan {
    width: 44px;
  }

  .meter-row {
    padding-right: 2px;
  }
</style>
