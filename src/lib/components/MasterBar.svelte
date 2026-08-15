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
  import { midiIo } from "../state/midiio.svelte";
  import { project } from "../state/project.svelte";
  import { formatDb, linToDb } from "../utils/format";
  import type { AudioDevice } from "../types/ipc";
  import Meter from "./Meter.svelte";

  let inputs = $state<AudioDevice[]>([]);
  let outputs = $state<AudioDevice[]>([]);
  let dbEl: HTMLSpanElement | undefined = $state();

  /** Note-out (ruling 10) can only address a MIDI track — the backend
   * rejects anything else, so the selector never offers it. */
  const midiTracks = $derived(project.tracks.filter((t) => t.kind === "midi"));

  const targetTrackName = $derived(
    midiIo.targetTrackId ? (project.trackById(midiIo.targetTrackId)?.name ?? midiIo.targetTrackId) : null,
  );

  onMount(() => {
    backend.listInputDevices().then((d) => (inputs = d)).catch(() => {});
    backend.listOutputDevices().then((d) => (outputs = d)).catch(() => {});
    void midiIo.loadPorts();
  });

  // Hardware MIDI I/O (slice 1/1b input, slice 2 output): port list/select,
  // a live activity dot, monitor toggle, input routing readout, output
  // port, clock toggle — all mirrored from `midiIo`'s poll (500 ms, mirrors
  // the mcp status poll cadence pattern in state/mcp.svelte.ts, just faster
  // since "activity" needs to feel live).
  $effect(() => midiIo.startPolling());

  function selectMidiInPort(id: string) {
    void midiIo.selectInPort(id);
  }

  function toggleMidiMonitor(enabled: boolean) {
    void midiIo.setMonitor(enabled);
  }

  function selectMidiOutPort(id: string) {
    void midiIo.selectOutPort(id);
  }

  function toggleClock(enabled: boolean) {
    void midiIo.setClockEnabled(enabled);
  }

  function selectMidiOutTrack(id: string) {
    void midiIo.setOutputTrack(id || null);
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
        onchange={(e) => selectMidiInPort((e.currentTarget as HTMLSelectElement).value)}
      >
        <option value="" selected={midiIo.inPortId === ""}>None</option>
        {#each midiIo.inPorts as p (p.id)}
          <option value={p.id} selected={p.id === midiIo.inPortId}>{p.name}</option>
        {/each}
      </select>
      <span class="midi-dot" class:active={midiIo.active} title="MIDI activity"></span>
      <label class="monitor" title="Hear incoming notes through a preview-grade voice (docs/midi-input.md)">
        <input
          type="checkbox"
          checked={midiIo.monitor}
          onchange={(e) => toggleMidiMonitor((e.currentTarget as HTMLInputElement).checked)}
        />
        <span class="silk">monitor</span>
      </label>
      {#if targetTrackName}
        <span class="silk target" title="Incoming notes are routed to this track's instrument">
          → {targetTrackName}
        </span>
      {/if}
    </div>
    <div class="dev">
      <span class="silk">midi out</span>
      <select
        name="midi-output-device"
        onchange={(e) => selectMidiOutPort((e.currentTarget as HTMLSelectElement).value)}
      >
        <option value="" selected={midiIo.outPortId === ""}>None</option>
        {#each midiIo.outPorts as p (p.id)}
          <option value={p.id} selected={p.id === midiIo.outPortId}>{p.name}</option>
        {/each}
      </select>
      <label class="monitor" title="Send MIDI clock + transport (Start/Stop/Continue/SPP) to the output port">
        <input
          type="checkbox"
          checked={midiIo.clockEnabled}
          onchange={(e) => toggleClock((e.currentTarget as HTMLInputElement).checked)}
        />
        <span class="silk">clock</span>
      </label>
      <span class="silk">trk</span>
      <select
        name="midi-output-track"
        title="Play this MIDI track's notes through the output port (mute it in AURA to avoid doubling)"
        onchange={(e) => selectMidiOutTrack((e.currentTarget as HTMLSelectElement).value)}
      >
        <option value="" selected={!midiIo.noteTrackId}>None</option>
        {#each midiTracks as t (t.id)}
          <option value={t.id} selected={t.id === midiIo.noteTrackId}>{t.name}</option>
        {/each}
      </select>
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
  .target {
    color: var(--cyan);
    white-space: nowrap;
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
