<script lang="ts">
  /**
   * Bottom master strip: big stereo peak-hold meter with scale, live dBFS
   * readout, audio I/O device selectors, a one-line MIDI status chip that
   * opens the patchbay, and the sidecar job console.
   *
   * The MIDI controls themselves live in the dock's patchbay panel
   * (components/midi/MidiPanel.svelte) — they never fit this row.
   */
  import { onMount } from "svelte";
  import { backend } from "../tauri";
  import { jobs } from "../state/jobs.svelte";
  import { generation, KIND_LABEL } from "../state/generation.svelte";
  import { latestMeter } from "../state/meters.svelte";
  import { midiIo } from "../state/midiio.svelte";
  import { project } from "../state/project.svelte";
  import { ui, DOCK_SHORTCUT } from "../state/ui.svelte";
  import { formatDb, linToDb } from "../utils/format";
  import { bulkTriState, bulkableTracks, fieldValues, nextBulkValue, type BulkField } from "../utils/lane-bulk";
  import type { AudioDevice } from "../types/ipc";
  import Meter from "./Meter.svelte";

  // Project-wide M/S/A (4.5): every track that has a mute/solo/arm control
  // at all — automation lanes don't, so they never count toward "all".
  const bulkTracks = $derived(bulkableTracks(project.tracks));
  const muteState = $derived(bulkTriState(fieldValues(bulkTracks, "muted")));
  const soloState = $derived(bulkTriState(fieldValues(bulkTracks, "soloed")));
  const armState = $derived(bulkTriState(fieldValues(bulkTracks, "armed")));

  function pressBulk(field: BulkField) {
    if (bulkTracks.length === 0) return;
    const value = nextBulkValue(fieldValues(bulkTracks, field));
    void project.setTracksState(
      bulkTracks.map((t) => t.id),
      field,
      value,
    );
  }

  let inputs = $state<AudioDevice[]>([]);
  let outputs = $state<AudioDevice[]>([]);
  let dbEl: HTMLSpanElement | undefined = $state();

  // What the MIDI chip reports: the input port that is listening, the track
  // its notes land on, and how many output ports are open.
  const inPortName = $derived(
    midiIo.inPortId
      ? (midiIo.inPorts.find((p) => p.id === midiIo.inPortId)?.name ?? midiIo.inPortId)
      : "no input",
  );

  const targetTrackName = $derived(
    midiIo.targetTrackId ? (project.trackById(midiIo.targetTrackId)?.name ?? midiIo.targetTrackId) : null,
  );

  onMount(() => {
    backend.listInputDevices().then((d) => (inputs = d)).catch(() => {});
    backend.listOutputDevices().then((d) => (outputs = d)).catch(() => {});
    void midiIo.loadPorts();
  });

  // The whole app's MIDI poll lives HERE, not in the patchbay panel: this
  // strip is always mounted, the dock panel is conditional. Move this and
  // the panel shows whatever was true when it was last open. 500 ms mirrors
  // the mcp status poll cadence in state/mcp.svelte.ts, just faster since
  // "activity" needs to feel live.
  $effect(() => midiIo.startPolling());

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

  <div class="section lanesbulk" role="group" aria-label="Mute, solo and arm every lane">
    <span class="silk">lanes</span>
    <button
      class="bulk mute"
      class:on={muteState === "on"}
      class:mixed={muteState === "mixed"}
      role="checkbox"
      aria-checked={muteState === "mixed" ? "mixed" : muteState === "on"}
      title="Mute all {bulkTracks.length} tracks"
      aria-label="Mute all tracks"
      onclick={() => pressBulk("muted")}>M</button
    >
    <button
      class="bulk solo"
      class:on={soloState === "on"}
      class:mixed={soloState === "mixed"}
      role="checkbox"
      aria-checked={soloState === "mixed" ? "mixed" : soloState === "on"}
      title="Solo all {bulkTracks.length} tracks"
      aria-label="Solo all tracks"
      onclick={() => pressBulk("soloed")}>S</button
    >
    <button
      class="bulk arm"
      class:on={armState === "on"}
      class:mixed={armState === "mixed"}
      role="checkbox"
      aria-checked={armState === "mixed" ? "mixed" : armState === "on"}
      title="Arm all {bulkTracks.length} tracks"
      aria-label="Arm all tracks"
      onclick={() => pressBulk("armed")}>A</button
    >
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
    <button
      class="dev midistat"
      type="button"
      title="MIDI patchbay — press {DOCK_SHORTCUT.midi.toUpperCase()}"
      onclick={() => (ui.dock = "midi")}
    >
      <span class="silk">midi</span>
      <span class="midi-dot" class:active={midiIo.active} aria-hidden="true"></span>
      <span class="patched mono">{inPortName}{targetTrackName ? ` → ${targetTrackName}` : ""}</span>
      <span class="tally mono">{midiIo.outputs.length} out</span>
      <span class="opener silk" aria-hidden="true">⇄</span>
    </button>
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
    /* The master strip is the bottom of the same chassis as the transport
       bar. Its cast shadow goes UP, onto the timeline above it, which is
       why it cannot just reuse `--relief-3`. */
    background-image: var(--sheen-face);
    box-shadow:
      inset 0 var(--border-width) 0 0 var(--bevel-hi),
      0 calc(-4px * var(--relief)) calc(16px * var(--relief))
        rgb(var(--shadow-rgb) / calc(var(--relief) * 0.4));
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

  .lanesbulk {
    gap: 6px;
    padding-left: 4px;
    border-left: var(--border-width) solid var(--glass-border);
  }
  .bulk {
    width: 20px;
    height: 20px;
    flex: none;
    font-family: var(--font-mono);
    font-size: 9px;
    border-radius: 4px;
    border: var(--border-width) solid var(--glass-border);
    background: rgb(var(--bg-0-rgb) / 0.7);
    color: var(--text-faint);
    cursor: pointer;
    padding: 0;
  }
  .bulk.mute.on {
    color: var(--bg-0);
    background: var(--amber);
    border-color: var(--amber);
  }
  .bulk.solo.on {
    color: var(--bg-0);
    background: var(--cyan);
    border-color: var(--cyan);
  }
  .bulk.arm.on {
    color: var(--text-on-accent);
    background: rgb(var(--red-rgb) / 0.8);
    border-color: var(--red);
  }
  /* Mixed: a quieter tell than full-on, same family as LaneGroupHeader's —
     tinted outline, no fill, so "some of these" never reads as "all of
     these". */
  .bulk.mute.mixed {
    color: var(--amber);
    border-style: dashed;
    border-color: var(--amber);
  }
  .bulk.solo.mixed {
    color: var(--cyan);
    border-style: dashed;
    border-color: var(--cyan);
  }
  .bulk.arm.mixed {
    color: var(--red);
    border-style: dashed;
    border-color: var(--red);
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

  /* One chip, fixed width: the 500 ms poll changes the text inside it, never
     the strip's layout. Focus ring comes from the global :focus-visible. */
  .midistat {
    width: 214px;
    padding: 3px 7px;
    border-radius: 4px;
    border: var(--border-width) solid var(--glass-border);
    background: rgb(var(--bg-0-rgb) / 0.7);
    color: var(--text-dim);
    font: inherit;
    text-align: left;
    cursor: pointer;
    transition: border-color 120ms linear, color 120ms linear;
  }
  .midistat:hover {
    color: var(--text);
    border-color: color-mix(in srgb, var(--cyan) 45%, transparent);
  }
  .patched {
    flex: 1;
    min-width: 0;
    font-size: 10px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .tally {
    flex: none;
    width: 40px;
    font-size: 10px;
    text-align: right;
    color: var(--text-faint);
  }
  .opener {
    flex: none;
    color: var(--cyan);
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
