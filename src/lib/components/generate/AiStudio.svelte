<script lang="ts">
  /**
   * AI Studio — prompt-driven generation over sidecar_run_job. One panel,
   * six job kinds, parameter forms with schema defaults, live NDJSON progress
   * in the house loading-wave style, and results that land as clips /
   * note-fills / instruments.
   */
  import { generation, KIND_LABEL } from "../../state/generation.svelte";
  import { project } from "../../state/project.svelte";
  import { midi } from "../../state/midi.svelte";
  import { ui } from "../../state/ui.svelte";
  import { formatBarsBeats } from "../../utils/format";

  const uid = () => Math.random().toString(36).slice(2, 8);

  interface KindDef {
    kind: string;
    name: string;
    blurb: string;
    needs: "" | "audioClip" | "midiClip";
  }
  const KINDS: KindDef[] = [
    { kind: "aceStepGenerate", name: "Generate", blurb: "text → music (ACE-Step, local)", needs: "" },
    { kind: "aceStepRepaint", name: "Repaint", blurb: "regenerate a span of a clip", needs: "audioClip" },
    { kind: "aceStepAudio2Audio", name: "Audio2Audio", blurb: "restyle a clip by prompt", needs: "audioClip" },
    { kind: "elevenLabsMusic", name: "ElevenLabs", blurb: "text → music (cloud API)", needs: "" },
    { kind: "amtInfill", name: "MIDI Infill", blurb: "fill a note region (AMT)", needs: "midiClip" },
    { kind: "stableAudioSfz", name: "Instrument", blurb: "text → SFZ multisample", needs: "" },
  ];

  const active = $derived(KINDS.find((k) => k.kind === ui.studioKind) ?? KINDS[0]);

  // ── form state (schema defaults) ──
  let prompt = $state("neon-lit synthwave, driving arps, rain on chrome");
  let lyrics = $state("");
  let durationSeconds = $state(30);
  let seed = $state("");
  let strength = $state(0.7);
  let repaintStart = $state(0);
  let repaintEnd = $state(8);
  let temperature = $state(1.0);
  let topP = $state(0.98);
  let instName = $state("chrome keys");
  let velocityLayers = $state(1);
  let importTrackId = $state("");

  const audioTracks = $derived(project.tracks.filter((t) => t.kind !== "midi"));
  const selAudioClip = $derived(project.selectedClip);
  const targetMidiClip = $derived(midi.openClip ?? midi.selectedClip);
  const region = $derived(
    midi.region && midi.region.clipId === targetMidiClip?.id ? midi.region : null,
  );

  const disabledReason = $derived.by(() => {
    if (active.needs === "audioClip" && !selAudioClip) return "select an audio clip on the timeline first";
    if (active.needs === "midiClip" && !targetMidiClip) return "select a MIDI clip first";
    return "";
  });

  function genDir(): string {
    return project.projectDir ? `${project.projectDir}/cache/gen` : "/tmp/aura-gen";
  }
  function absSource(path: string): string {
    if (path.startsWith("/")) return path;
    return project.projectDir ? `${project.projectDir}/${path}` : `/${path}`;
  }

  function seedParam(): Record<string, unknown> {
    const n = parseInt(seed, 10);
    return Number.isFinite(n) ? { seed: n } : {};
  }

  async function run() {
    if (disabledReason) return;
    const k = active.kind;
    const label = prompt.slice(0, 42) || active.name;

    if (k === "amtInfill") {
      const clip = targetMidiClip!;
      const startTicks = region?.startTicks ?? 0;
      const endTicks = region?.endTicks ?? clip.lengthTicks;
      const contextNotes = clip.notes.filter(
        (n) => n.tick + n.lengthTicks <= startTicks || n.tick >= endTicks,
      );
      await generation.run(
        k,
        {
          ppq: midi.ppq,
          contextNotes,
          regionStartTicks: startTicks,
          regionEndTicks: endTicks,
          temperature,
          topP,
        },
        `infill · ${clip.name}`,
        { clipId: clip.id, startTicks, endTicks },
      );
      return;
    }

    if (k === "stableAudioSfz") {
      await generation.run(
        k,
        {
          prompt,
          name: instName || "generated",
          outputDir: `${genDir()}/sfz-${uid()}`,
          keys: [36, 39, 42, 45, 48, 51, 54, 57, 60, 63, 66, 69, 72],
          velocityLayers,
          ...seedParam(),
        },
        `instrument · ${instName || prompt.slice(0, 24)}`,
      );
      return;
    }

    const outputPath = `${genDir()}/${k}-${uid()}.wav`;
    const importParams = { importToTrackId: importTrackId || null, importAtSamples: 0 };
    if (k === "aceStepGenerate") {
      await generation.run(
        k,
        { prompt, ...(lyrics ? { lyrics } : {}), durationSeconds, outputPath, ...seedParam(), ...importParams },
        label,
      );
    } else if (k === "elevenLabsMusic") {
      await generation.run(k, { prompt, durationSeconds, outputPath, ...importParams }, label);
    } else if (k === "aceStepRepaint") {
      await generation.run(
        k,
        {
          prompt,
          inputPath: absSource(selAudioClip!.sourcePath),
          startSeconds: repaintStart,
          endSeconds: repaintEnd,
          outputPath,
          ...seedParam(),
          ...importParams,
        },
        `repaint · ${selAudioClip!.name}`,
      );
    } else if (k === "aceStepAudio2Audio") {
      await generation.run(
        k,
        {
          prompt,
          inputPath: absSource(selAudioClip!.sourcePath),
          strength,
          outputPath,
          ...seedParam(),
          ...importParams,
        },
        `a2a · ${selAudioClip!.name}`,
      );
    }
  }

  const jobsNewestFirst = $derived(generation.all.slice(0, 6));

  function regionLabel(): string {
    if (!targetMidiClip) return "";
    const start = region?.startTicks ?? 0;
    const end = region?.endTicks ?? targetMidiClip.lengthTicks;
    const bars = (t: number) => (t / midi.ticksPerBar + 1).toFixed(2).replace(/\.00$/, "");
    return `bars ${bars(start)} – ${bars(end)}${region ? "" : " (whole clip — shift-drag in the roll to narrow)"}`;
  }
  void formatBarsBeats; // (bars helper above is tick-based)
</script>

<div class="studio">
  <div class="kinds" role="tablist" aria-label="Job kind">
    {#each KINDS as k (k.kind)}
      <button
        class="kind"
        class:on={active.kind === k.kind}
        role="tab"
        aria-selected={active.kind === k.kind}
        onclick={() => (ui.studioKind = k.kind)}
      >
        <span class="kname">{k.name}</span>
        <span class="kblurb silk">{k.blurb}</span>
      </button>
    {/each}
  </div>

  <div class="form">
    {#if active.kind !== "amtInfill"}
      <label class="field">
        <span class="silk">prompt</span>
        <textarea rows="2" bind:value={prompt} placeholder="describe the sound…"></textarea>
      </label>
    {/if}

    {#if active.kind === "aceStepGenerate"}
      <label class="field">
        <span class="silk">lyrics · optional</span>
        <textarea rows="2" bind:value={lyrics} placeholder="[verse] …"></textarea>
      </label>
    {/if}

    {#if active.kind === "aceStepGenerate" || active.kind === "elevenLabsMusic"}
      <div class="row">
        <label class="field small">
          <span class="silk">duration s</span>
          <input type="number" min="1" max={active.kind === "elevenLabsMusic" ? 300 : 600} bind:value={durationSeconds} />
        </label>
        {#if active.kind === "aceStepGenerate"}
          <label class="field small">
            <span class="silk">seed</span>
            <input type="text" placeholder="random" bind:value={seed} />
          </label>
        {/if}
      </div>
    {/if}

    {#if active.needs === "audioClip"}
      <div class="context mono" class:missing={!selAudioClip}>
        {#if selAudioClip}
          ◈ {selAudioClip.name}
        {:else}
          ◈ no audio clip selected
        {/if}
      </div>
      {#if active.kind === "aceStepRepaint"}
        <div class="row">
          <label class="field small">
            <span class="silk">start s</span>
            <input type="number" min="0" step="0.5" bind:value={repaintStart} />
          </label>
          <label class="field small">
            <span class="silk">end s</span>
            <input type="number" min="0.5" step="0.5" bind:value={repaintEnd} />
          </label>
        </div>
      {:else if active.kind === "aceStepAudio2Audio"}
        <label class="field">
          <span class="silk">strength · {strength.toFixed(2)} (0 keep · 1 ignore)</span>
          <input class="fader" type="range" min="0" max="1" step="0.01" bind:value={strength}
            style:--fader-pct="{strength * 100}%" />
        </label>
      {/if}
    {/if}

    {#if active.kind === "amtInfill"}
      <div class="context mono" class:missing={!targetMidiClip}>
        {#if targetMidiClip}
          ▦ {targetMidiClip.name} · {regionLabel()}
        {:else}
          ▦ no MIDI clip selected
        {/if}
      </div>
      <div class="row">
        <label class="field small">
          <span class="silk">temperature</span>
          <input type="number" min="0.1" max="2" step="0.05" bind:value={temperature} />
        </label>
        <label class="field small">
          <span class="silk">top-p</span>
          <input type="number" min="0.1" max="1" step="0.01" bind:value={topP} />
        </label>
      </div>
    {/if}

    {#if active.kind === "stableAudioSfz"}
      <div class="row">
        <label class="field">
          <span class="silk">instrument name</span>
          <input type="text" bind:value={instName} />
        </label>
        <label class="field small">
          <span class="silk">vel layers</span>
          <input type="number" min="1" max="4" bind:value={velocityLayers} />
        </label>
      </div>
      <div class="context mono">⌁ multisampled C2–C6 · minor thirds · SFZ subset v1</div>
    {/if}

    {#if active.kind !== "amtInfill" && active.kind !== "stableAudioSfz"}
      <label class="field">
        <span class="silk">result → track</span>
        <select bind:value={importTrackId}>
          <option value="">new track (auto)</option>
          {#each audioTracks as t (t.id)}
            <option value={t.id}>{t.name}</option>
          {/each}
        </select>
      </label>
    {/if}

    <button class="run mono" disabled={!!disabledReason} title={disabledReason || "Run the job"} onclick={run}>
      <span class="spark">✦</span>
      {disabledReason ? disabledReason.toUpperCase() : `RUN ${KIND_LABEL[active.kind] ?? active.kind}`}
    </button>
  </div>

  <div class="joblist">
    <span class="silk">jobs</span>
    {#if jobsNewestFirst.length === 0}
      <div class="empty silk">nothing generated yet — write a prompt and run</div>
    {/if}
    {#each jobsNewestFirst as job (job.jobId)}
      <div class="job" class:err={job.state === "error"} class:live={job.state === "running" || job.state === "queued"}>
        {#if job.state === "running" || job.state === "queued"}
          <div class="wavebox"><div class="scan"></div><div class="scan lag"></div></div>
        {/if}
        <div class="jhead">
          <span class="jkind mono">{KIND_LABEL[job.kind] ?? job.kind}</span>
          <span class="jlabel">{job.label}</span>
          {#if job.state === "running" || job.state === "queued"}
            <span class="jpct mono">{Math.round(job.progress * 100)}%</span>
            <button class="x mono" title="Cancel" onclick={() => generation.cancel(job.jobId)}>×</button>
          {:else}
            <span class="jstate mono {job.state}">{job.state.toUpperCase()}</span>
            <button class="x mono" title="Dismiss" onclick={() => generation.dismiss(job.jobId)}>×</button>
          {/if}
        </div>
        {#if job.state === "running" || job.state === "queued"}
          <div class="jstage silk">{job.stage}</div>
          <div class="pbar"><div class="pfill" style:width="{job.progress * 100}%"></div></div>
        {:else if job.outcome}
          <div class="jstage silk done">↳ {job.outcome}</div>
        {:else if job.error}
          <div class="jstage silk errtext">{job.error}</div>
        {/if}
        {#if job.log.length && (job.state === "running" || job.state === "error")}
          <pre class="log mono">{job.log.slice(-4).join("\n")}</pre>
        {/if}
      </div>
    {/each}
  </div>
</div>

<style>
  .studio {
    display: flex;
    flex-direction: column;
    gap: 14px;
    min-height: 0;
    flex: 1;
    overflow-y: auto;
    padding-right: 2px;
  }

  .kinds {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 6px;
  }
  .kind {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 2px;
    padding: 7px 9px;
    border-radius: 5px;
    border: 1px solid var(--glass-border);
    background: rgba(16, 20, 42, 0.35);
    cursor: pointer;
    text-align: left;
    transition: border-color 120ms, box-shadow 120ms;
  }
  .kind:hover {
    border-color: rgba(122, 160, 220, 0.3);
  }
  .kind.on {
    border-color: var(--cyan-dim);
    box-shadow: 0 0 10px rgba(82, 229, 255, 0.15);
  }
  .kname {
    font-size: 11px;
    color: var(--text);
  }
  .kind.on .kname {
    color: var(--cyan);
  }
  .kblurb {
    letter-spacing: 0.08em;
  }

  .form {
    display: flex;
    flex-direction: column;
    gap: 9px;
  }
  .row {
    display: flex;
    gap: 9px;
  }
  .field {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .field.small {
    flex: 0 0 88px;
  }
  textarea,
  input[type="text"],
  input[type="number"],
  select {
    background: rgba(5, 7, 13, 0.7);
    color: var(--text);
    border: 1px solid var(--glass-border);
    border-radius: 4px;
    font-size: 11px;
    font-family: var(--font-ui);
    padding: 5px 7px;
    resize: vertical;
  }
  textarea:focus,
  input:focus,
  select:focus {
    border-color: var(--cyan-dim);
  }

  .context {
    font-size: 10px;
    color: var(--cyan);
    background: rgba(82, 229, 255, 0.05);
    border: 1px dashed rgba(82, 229, 255, 0.25);
    border-radius: 4px;
    padding: 5px 8px;
  }
  .context.missing {
    color: var(--amber);
    background: rgba(255, 200, 87, 0.05);
    border-color: rgba(255, 200, 87, 0.3);
  }

  .run {
    margin-top: 2px;
    padding: 8px 10px;
    font-size: 10px;
    letter-spacing: 0.16em;
    border: none;
    border-radius: 5px;
    cursor: pointer;
    color: var(--bg-0);
    background: linear-gradient(100deg, var(--cyan), var(--magenta));
    box-shadow:
      0 0 14px rgba(82, 229, 255, 0.3),
      0 0 22px rgba(255, 79, 216, 0.2);
    transition: filter 120ms;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .run:hover:not(:disabled) {
    filter: brightness(1.15);
  }
  .run:disabled {
    filter: saturate(0.15) brightness(0.7);
    cursor: not-allowed;
  }
  .spark {
    margin-right: 4px;
  }

  .joblist {
    display: flex;
    flex-direction: column;
    gap: 7px;
  }
  .empty {
    padding: 12px 0;
  }
  .job {
    position: relative;
    border: 1px solid var(--glass-border);
    border-radius: 6px;
    padding: 7px 9px 8px;
    background: rgba(10, 13, 23, 0.6);
    overflow: hidden;
  }
  .job.live {
    border-color: color-mix(in srgb, var(--cyan) 45%, var(--magenta) 25%);
  }
  .job.err {
    border-color: rgba(255, 65, 82, 0.4);
  }

  .wavebox {
    position: absolute;
    inset: 0;
    pointer-events: none;
    opacity: 0.5;
  }
  .scan {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 34%;
    background: linear-gradient(
      90deg,
      transparent,
      rgba(82, 229, 255, 0.12) 35%,
      rgba(82, 229, 255, 0.3) 50%,
      rgba(255, 79, 216, 0.2) 65%,
      transparent
    );
    mix-blend-mode: screen;
    animation: scan-sweep 1.7s linear infinite;
  }
  .scan.lag {
    animation-delay: -0.85s;
    opacity: 0.5;
    filter: hue-rotate(80deg);
  }
  @keyframes scan-sweep {
    from {
      transform: translateX(-110%);
      left: 0;
    }
    to {
      transform: translateX(110%);
      left: 66%;
    }
  }

  .jhead {
    display: flex;
    align-items: center;
    gap: 8px;
    position: relative;
  }
  .jkind {
    font-size: 8px;
    letter-spacing: 0.15em;
    color: var(--magenta);
    white-space: nowrap;
  }
  .jlabel {
    flex: 1;
    min-width: 0;
    font-size: 11px;
    color: var(--text);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .jpct {
    font-size: 10px;
    color: var(--cyan);
  }
  .jstate {
    font-size: 8px;
    letter-spacing: 0.15em;
  }
  .jstate.done {
    color: #5cf2b8;
  }
  .jstate.error,
  .jstate.cancelled {
    color: var(--red);
  }
  .x {
    border: none;
    background: transparent;
    color: var(--text-faint);
    cursor: pointer;
    font-size: 12px;
    padding: 0 2px;
  }
  .x:hover {
    color: var(--red);
  }

  .jstage {
    margin-top: 3px;
    color: var(--cyan);
    position: relative;
  }
  .jstage.done {
    color: #5cf2b8;
  }
  .jstage.errtext {
    color: var(--red);
    text-transform: none;
    letter-spacing: 0.05em;
  }
  .pbar {
    margin-top: 5px;
    height: 3px;
    border-radius: 2px;
    background: rgba(96, 130, 190, 0.15);
    overflow: hidden;
    position: relative;
  }
  .pfill {
    height: 100%;
    background: linear-gradient(90deg, var(--cyan), var(--magenta));
    box-shadow: 0 0 8px rgba(82, 229, 255, 0.5);
    transition: width 180ms linear;
  }

  .log {
    position: relative;
    margin: 6px 0 0;
    padding: 5px 7px;
    font-size: 9px;
    line-height: 1.5;
    color: var(--text-dim);
    background: rgba(5, 7, 13, 0.75);
    border-radius: 4px;
    border: 1px solid rgba(96, 130, 190, 0.1);
    white-space: pre-wrap;
    word-break: break-all;
    max-height: 64px;
    overflow: hidden;
  }
</style>
