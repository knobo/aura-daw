<script lang="ts">
  /**
   * Bottom master strip: big stereo peak-hold meter with scale, live dBFS
   * readout, I/O device selectors and the sidecar job console.
   */
  import { onMount } from "svelte";
  import { backend } from "../tauri";
  import { jobs } from "../state/jobs.svelte";
  import { generation, KIND_LABEL } from "../state/generation.svelte";
  import { latestMeter } from "../state/meters.svelte";
  import { formatDb, linToDb } from "../utils/format";
  import type { AudioDevice, MidiPortInfo } from "../types/ipc";
  import Meter from "./Meter.svelte";

  let inputs = $state<AudioDevice[]>([]);
  let outputs = $state<AudioDevice[]>([]);
  let dbEl: HTMLSpanElement | undefined = $state();

  // Hardware MIDI input (midi-input-ports slice 1/1b): port list/select, a
  // live activity dot, and a "monitor" toggle that makes incoming notes
  // audible through a preview-grade voice (see docs/midi-input.md).
  // Optional backend methods — no-op (empty list, dot never lights) in
  // demo mode, which has no hardware to simulate.
  let midiPorts = $state<MidiPortInfo[]>([]);
  let midiSelectedId = $state("");
  let midiActive = $state(false);
  let midiMonitor = $state(true); // default ON, matches the backend default

  onMount(() => {
    backend.listInputDevices().then((d) => (inputs = d)).catch(() => {});
    backend.listOutputDevices().then((d) => (outputs = d)).catch(() => {});
    backend.midiListInputPorts?.().then((p) => (midiPorts = p)).catch(() => {});
  });

  // Poll live MIDI status at ~2 Hz while this bar is mounted (mirrors the
  // mcp status poll cadence pattern in state/mcp.svelte.ts, just faster
  // since "activity" needs to feel live).
  $effect(() => {
    if (!backend.midiInputStatus) return;
    const id = setInterval(() => {
      backend.midiInputStatus?.().then((s) => {
        midiSelectedId = s.selected?.id ?? "";
        midiActive = s.lastEventAgeMs != null && s.lastEventAgeMs < 300;
        if (s.selected) midiMonitor = s.monitor;
      }).catch(() => {});
    }, 500);
    return () => clearInterval(id);
  });

  function selectMidiPort(id: string) {
    midiSelectedId = id;
    backend.midiSelectInputPort?.(id || null, midiMonitor);
  }

  function toggleMidiMonitor(enabled: boolean) {
    midiMonitor = enabled;
    if (midiSelectedId) backend.midiSelectInputPort?.(midiSelectedId, enabled);
  }

  // live dB readout without reactivity churn
  $effect(() => {
    let raf = 0;
    let shown = -100;
    const tick = () => {
      raf = requestAnimationFrame(tick);
      if (!dbEl) return;
      const m = latestMeter("master");
      const db = m ? Math.max(linToDb(m.peakL), linToDb(m.peakR)) : -100;
      shown = db > shown ? db : Math.max(db, shown - 0.5);
      dbEl.textContent = formatDb(shown);
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  });

  const activeJobs = $derived(jobs.active);
  const activeGenJobs = $derived(generation.active);
</script>

<footer class="master glass">
  <div class="section meterblock">
    <span class="silk">master</span>
    <div class="meterwrap">
      <Meter trackId="master" height={22} scale />
    </div>
    <span class="db mono" bind:this={dbEl}>-∞</span>
  </div>

  <div class="section io">
    <label class="dev">
      <span class="silk">input</span>
      <select name="input-device" onchange={(e) => backend.selectInputDevice((e.currentTarget as HTMLSelectElement).value)}>
        {#each inputs as d (d.id)}
          <option value={d.id} selected={d.isDefault}>{d.name}</option>
        {/each}
      </select>
    </label>
    <label class="dev">
      <span class="silk">output</span>
      <select name="output-device" onchange={(e) => backend.selectOutputDevice((e.currentTarget as HTMLSelectElement).value)}>
        {#each outputs as d (d.id)}
          <option value={d.id} selected={d.isDefault}>{d.name}</option>
        {/each}
      </select>
    </label>
    <div class="dev">
      <span class="silk">midi in</span>
      <select
        name="midi-input-device"
        onchange={(e) => selectMidiPort((e.currentTarget as HTMLSelectElement).value)}
      >
        <option value="" selected={midiSelectedId === ""}>None</option>
        {#each midiPorts as p (p.id)}
          <option value={p.id} selected={p.id === midiSelectedId}>{p.name}</option>
        {/each}
      </select>
      <span class="midi-dot" class:active={midiActive} title="MIDI activity"></span>
      <label class="monitor" title="Hear incoming notes through a preview-grade voice (docs/midi-input.md)">
        <input
          type="checkbox"
          checked={midiMonitor}
          onchange={(e) => toggleMidiMonitor((e.currentTarget as HTMLInputElement).checked)}
        />
        <span class="silk">monitor</span>
      </label>
    </div>
  </div>

  <div class="section jobsbox">
    <span class="silk">jobs</span>
    {#if activeJobs.length === 0 && activeGenJobs.length === 0}
      <span class="idle mono">idle</span>
    {:else}
      {#each activeJobs as job (job.jobId)}
        <div class="job mono" title={job.stage}>
          <span class="jkind">{job.kind === "stemSplit" ? "STEMS" : "WHISPER"}</span>
          <div class="jbar">
            <div class="jfill" style:width="{job.progress * 100}%"></div>
          </div>
          <span class="jpct">{Math.round(job.progress * 100)}%</span>
        </div>
      {/each}
      {#each activeGenJobs as job (job.jobId)}
        <div class="job mono" title="{job.label} — {job.stage}">
          {#if job.origin === "agent"}
            <span class="jagent" title="started by an agent over MCP">MCP</span>
          {/if}
          <span class="jkind">{KIND_LABEL[job.kind] ?? job.kind}</span>
          <div class="jbar">
            <div class="jfill" style:width="{job.progress * 100}%"></div>
          </div>
          <span class="jpct">{Math.round(job.progress * 100)}%</span>
        </div>
      {/each}
    {/if}
  </div>
</footer>

<style>
  .master {
    display: flex;
    align-items: center;
    gap: 26px;
    height: 58px;
    padding: 0 16px;
    border-left: none;
    border-right: none;
    border-bottom: none;
    position: relative;
    z-index: 20;
  }

  .section {
    display: flex;
    align-items: center;
    gap: 10px;
  }

  .meterblock {
    flex: none;
  }
  .meterwrap {
    width: 320px;
  }
  .db {
    font-size: 13px;
    width: 48px;
    color: var(--text);
    text-align: right;
  }

  .io {
    gap: 16px;
  }
  .dev {
    display: flex;
    align-items: center;
    gap: 7px;
  }
  .dev select {
    max-width: 190px;
    background: rgba(5, 7, 13, 0.7);
    color: var(--text-dim);
    border: 1px solid var(--glass-border);
    border-radius: 4px;
    font-size: 11px;
    padding: 3px 6px;
  }
  .midi-dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: rgba(96, 130, 190, 0.25);
    border: 1px solid var(--glass-border);
    transition: background 80ms linear, box-shadow 80ms linear;
  }
  .midi-dot.active {
    background: var(--cyan);
    box-shadow: 0 0 6px 1px var(--cyan);
  }
  .monitor {
    display: flex;
    align-items: center;
    gap: 4px;
    cursor: pointer;
  }
  .monitor input {
    accent-color: var(--cyan);
    width: 11px;
    height: 11px;
    margin: 0;
    cursor: pointer;
  }

  .jobsbox {
    flex: 1;
    min-width: 0;
    justify-content: flex-end;
  }
  .idle {
    font-size: 10px;
    color: var(--text-faint);
  }
  .job {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 3px 9px;
    border-radius: 4px;
    border: 1px solid var(--glass-border);
    background: rgba(5, 7, 13, 0.6);
  }
  .jkind {
    font-size: 9px;
    letter-spacing: 0.15em;
    color: var(--magenta);
  }
  .jagent {
    font-size: 8px;
    letter-spacing: 0.12em;
    padding: 1px 3px;
    border-radius: 2px;
    color: var(--cyan);
    border: 1px solid color-mix(in srgb, var(--cyan) 45%, transparent);
  }
  .jbar {
    width: 90px;
    height: 3px;
    border-radius: 2px;
    background: rgba(96, 130, 190, 0.18);
    overflow: hidden;
  }
  .jfill {
    height: 100%;
    background: linear-gradient(90deg, var(--cyan), var(--magenta));
    transition: width 180ms linear;
  }
  .jpct {
    font-size: 10px;
    color: var(--text-dim);
    width: 32px;
  }
</style>
