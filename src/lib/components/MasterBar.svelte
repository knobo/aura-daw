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

  // Output ports not currently open — the "+ add" dropdown's choices.
  const closedOutPorts = $derived(midiIo.outPorts.filter((p) => !midiIo.outputs.some((o) => o.port.id === p.id)));

  // Per-track routing: one MIDI track is "being edited" at a time in this
  // compact strip; its current route (if any) drives the port/channel
  // controls next to the track picker.
  let editingTrackId = $state("");
  const editingRoute = $derived(editingTrackId ? midiIo.trackRoute(editingTrackId) : undefined);

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

  function openOutputPort(id: string) {
    if (id) void midiIo.openOutPort(id);
  }

  function closeOutputPort(id: string) {
    void midiIo.closeOutPort(id);
  }

  function toggleClock(portId: string, enabled: boolean) {
    void midiIo.setClockEnabled(portId, enabled);
  }

  function selectRoutePort(portId: string) {
    if (!editingTrackId) return;
    void midiIo.setTrackRoute(editingTrackId, portId || null, editingRoute?.channel ?? 0);
  }

  function selectRouteChannel(channel: number) {
    if (!editingTrackId || !editingRoute) return;
    void midiIo.setTrackRoute(editingTrackId, editingRoute.portId, channel);
  }

  function selectRouteReturn(deviceId: string) {
    if (!editingTrackId || !editingRoute) return;
    void midiIo.setTrackReturn(editingTrackId, deviceId || null);
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
    <div class="dev midiout">
      <span class="silk">midi out</span>
      {#each midiIo.outputs as o (o.port.id)}
        <span class="portchip" title={o.port.name}>
          <span class="pname mono">{o.port.name}</span>
          <label class="monitor" title="Send MIDI clock + transport (Start/Stop/Continue/SPP) to this port">
            <input
              type="checkbox"
              checked={o.clockEnabled}
              onchange={(e) => toggleClock(o.port.id, (e.currentTarget as HTMLInputElement).checked)}
            />
            <span class="silk">clock</span>
          </label>
          <button
            type="button"
            class="portclose"
            title="Close this output port"
            onclick={() => closeOutputPort(o.port.id)}
          >×</button>
        </span>
      {/each}
      {#if closedOutPorts.length > 0}
        <select
          name="midi-output-add"
          title="Open an additional MIDI output port"
          onchange={(e) => {
            openOutputPort((e.currentTarget as HTMLSelectElement).value);
            (e.currentTarget as HTMLSelectElement).value = "";
          }}
        >
          <option value="" selected>+ open…</option>
          {#each closedOutPorts as p (p.id)}
            <option value={p.id}>{p.name}</option>
          {/each}
        </select>
      {/if}
      <span class="silk">trk</span>
      <select
        name="midi-output-track"
        title="Configure which port+channel this MIDI track's notes are routed to (mute it in AURA to avoid doubling)"
        onchange={(e) => (editingTrackId = (e.currentTarget as HTMLSelectElement).value)}
      >
        <option value="" selected={!editingTrackId}>None</option>
        {#each midiTracks as t (t.id)}
          <option value={t.id} selected={t.id === editingTrackId}>{t.name}</option>
        {/each}
      </select>
      {#if editingTrackId}
        <select
          name="midi-output-track-port"
          title="Port this track's notes are routed to"
          onchange={(e) => selectRoutePort((e.currentTarget as HTMLSelectElement).value)}
        >
          <option value="" selected={!editingRoute}>None</option>
          {#each midiIo.outputs as o (o.port.id)}
            <option value={o.port.id} selected={o.port.id === editingRoute?.portId}>{o.port.name}</option>
          {/each}
        </select>
        {#if editingRoute}
          <input
            class="channel mono"
            type="number"
            min="0"
            max="15"
            title="MIDI channel (0-15)"
            value={editingRoute.channel}
            onchange={(e) => selectRouteChannel(Math.max(0, Math.min(15, (e.currentTarget as HTMLInputElement).valueAsNumber || 0)))}
          />
          <select
            name="midi-output-track-return"
            title="Audio input this track records its return from (so export hears the external synth). Patch the synth into this input yourself."
            onchange={(e) => selectRouteReturn((e.currentTarget as HTMLSelectElement).value)}
          >
            <option value="" selected={!editingRoute.returnDevice}>return: none</option>
            {#each inputs as d (d.id)}
              <option value={d.id} selected={d.id === editingRoute.returnDevice}>{d.name}</option>
            {/each}
          </select>
        {/if}
      {/if}
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
    background: rgb(var(--bg-0-rgb) / 0.7);
    color: var(--text-dim);
    border: var(--border-width) solid var(--glass-border);
    border-radius: 4px;
    font-size: 11px;
    padding: 3px 6px;
  }
  .midi-dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: rgb(var(--line-rgb) / 0.25);
    border: 1px solid var(--glass-border);
    transition: background 80ms linear, box-shadow 80ms linear;
  }
  .midi-dot.active {
    background: var(--cyan);
    box-shadow: 0 0 calc(6px * var(--glow-scale)) 1px var(--cyan);
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

  .midiout {
    flex-wrap: wrap;
    row-gap: 4px;
  }
  .portchip {
    display: flex;
    align-items: center;
    gap: 5px;
    padding: 2px 6px;
    border-radius: 4px;
    border: var(--border-width) solid var(--glass-border);
    background: rgb(var(--bg-0-rgb) / 0.7);
  }
  .pname {
    font-size: 10px;
    color: var(--text-dim);
    max-width: 110px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .portclose {
    all: unset;
    cursor: pointer;
    font-size: 11px;
    line-height: 1;
    color: var(--text-faint);
    padding: 0 2px;
  }
  .portclose:hover {
    color: var(--text);
  }
  .channel {
    width: 34px;
    background: rgb(var(--bg-0-rgb) / 0.7);
    color: var(--text-dim);
    border: var(--border-width) solid var(--glass-border);
    border-radius: 4px;
    font-size: 11px;
    padding: 3px 4px;
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
    border: var(--border-width) solid var(--glass-border);
    background: rgb(var(--bg-0-rgb) / 0.6);
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
    background: rgb(var(--line-rgb) / 0.18);
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
