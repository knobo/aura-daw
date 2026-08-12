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
  import type { AudioDevice } from "../types/ipc";
  import Meter from "./Meter.svelte";

  let inputs = $state<AudioDevice[]>([]);
  let outputs = $state<AudioDevice[]>([]);
  let dbEl: HTMLSpanElement | undefined = $state();

  onMount(() => {
    backend.listInputDevices().then((d) => (inputs = d)).catch(() => {});
    backend.listOutputDevices().then((d) => (outputs = d)).catch(() => {});
  });

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
