<script lang="ts">
  /**
   * 🎤 HUM A MELODY — guided hum-to-song flow: record (existing transport
   * record surface) or pick an existing audio clip / WAV path, tune the
   * quantize/tempo/placement options, run `hum_to_song`, watch the staged
   * analysis (loadingAudio → trackingPitch → segmenting → quantizing), then
   * bind an instrument to the landed melody clip (Zyn plucked/lead patch
   * one-click when available).
   */
  import { backend } from "../../tauri";
  import { hum } from "../../state/hum.svelte";
  import { project } from "../../state/project.svelte";
  import { transport } from "../../state/transport.svelte";
  import { midi } from "../../state/midi.svelte";
  import { instruments } from "../../state/instruments.svelte";
  import { plugins } from "../../state/plugins.svelte";
  import { zyn } from "../../state/zynpatches.svelte";

  type SourceMode = "clip" | "path";
  let sourceMode = $state<SourceMode>("clip");
  let clipId = $state("");
  let filePath = $state("");
  let quantizeGrid = $state(0);
  let bpm = $state(""); // "" = project tempo
  let targetTrackId = $state(""); // "" = auto-create
  let atBar = $state(1);
  let accompany = $state(false);
  let zynBusy = $state(false);

  // newest audio clips first — a fresh take lands on top
  const audioClips = $derived([...project.clips].reverse());
  const midiTracks = $derived(project.tracks.filter((t) => t.kind === "midi"));
  const anyArmed = $derived(project.tracks.some((t) => t.armed));

  const STAGE_LABEL: Record<string, string> = {
    loadingAudio: "loading audio",
    trackingPitch: "tracking pitch",
    segmenting: "segmenting notes",
    quantizing: "quantizing to the grid",
  };

  const disabledReason = $derived.by(() => {
    if (hum.busy) return "";
    if (sourceMode === "clip" && !clipId) return "pick a recording first";
    if (sourceMode === "path" && !filePath.trim().startsWith("/"))
      return "enter an absolute wav path";
    return "";
  });

  async function run() {
    if (disabledReason || hum.busy) return;
    const bpmNum = parseFloat(bpm);
    await hum.run({
      clipId: sourceMode === "clip" ? clipId : null,
      path: sourceMode === "path" ? filePath.trim() : null,
      trackId: targetTrackId || null,
      atTicks: Math.max(0, Math.round((atBar - 1) * midi.ticksPerBar)),
      quantizeGrid: quantizeGrid || null,
      bpm: Number.isFinite(bpmNum) && bpmNum > 0 ? bpmNum : null,
      accompany,
    });
  }

  async function toggleRecord() {
    if (transport.isRecording) {
      const clips = await backend.stopRecording();
      await transport.pause();
      if (clips.length > 0) {
        sourceMode = "clip";
        clipId = clips[0].id;
      }
    } else {
      await backend.startRecording(null);
      await transport.init();
    }
  }

  async function bindZyn() {
    if (!hum.melodyTrackId || zynBusy) return;
    zynBusy = true;
    try {
      await zyn.bindZynVoice(hum.melodyTrackId);
      await project.reload();
    } finally {
      zynBusy = false;
    }
  }

  async function bindInstrument(e: Event) {
    const sel = e.currentTarget as HTMLSelectElement;
    const v = sel.value;
    sel.value = "";
    if (!v || !hum.melodyTrackId) return;
    if (v.startsWith("plugin:")) {
      await plugins.bind(v.slice("plugin:".length), hum.melodyTrackId);
    } else {
      await instruments.assign(hum.melodyTrackId, v);
    }
  }
</script>

<div class="panel">
  <div class="intro">
    <div class="lead mono">🎤 HUM A MELODY</div>
    <div class="blurb silk">sing or hum → pitch-tracked midi melody on a track</div>
  </div>

  {#if hum.phase === "idle" || hum.phase === "error"}
    <!-- step 1: source -->
    <div class="step">
      <span class="stepno mono">1</span>
      <span class="silk">source recording</span>
    </div>

    <div class="recrow">
      <button
        class="rec mono"
        class:recording={transport.isRecording}
        disabled={!anyArmed && !transport.isRecording}
        title={anyArmed
          ? "Record a hum on the armed track, stop to use the take"
          : "Arm a track in the rail first, then record your hum here"}
        onclick={toggleRecord}
      >
        {transport.isRecording ? "■ STOP TAKE" : "● RECORD A HUM"}
      </button>
      {#if !anyArmed && !transport.isRecording}
        <span class="hint silk">arm a track first</span>
      {/if}
    </div>

    <div class="srctabs">
      <button class="srctab mono" class:on={sourceMode === "clip"} onclick={() => (sourceMode = "clip")}>
        EXISTING CLIP
      </button>
      <button class="srctab mono" class:on={sourceMode === "path"} onclick={() => (sourceMode = "path")}>
        WAV PATH
      </button>
    </div>

    {#if sourceMode === "clip"}
      <select bind:value={clipId} title="Recorded/imported audio clip to analyze">
        <option value="">select a recording…</option>
        {#each audioClips as c (c.id)}
          <option value={c.id}>{c.name} · {(c.lengthSamples / c.sourceSampleRate).toFixed(1)}s</option>
        {/each}
      </select>
    {:else}
      <input
        type="text"
        class="mono"
        placeholder="/absolute/path/to/hum.wav"
        bind:value={filePath}
        spellcheck="false"
      />
    {/if}

    <!-- step 2: options -->
    <div class="step">
      <span class="stepno mono">2</span>
      <span class="silk">melody options</span>
    </div>

    <div class="row">
      <label class="field">
        <span class="silk">quantize</span>
        <select bind:value={quantizeGrid}>
          <option value={0}>off (free)</option>
          <option value={4}>1/4 notes</option>
          <option value={8}>1/8 notes</option>
          <option value={16}>1/16 notes</option>
        </select>
      </label>
      <label class="field small">
        <span class="silk">bpm</span>
        <input type="text" placeholder={project.tempoBpm.toFixed(0)} bind:value={bpm} />
      </label>
      <label class="field small">
        <span class="silk">at bar</span>
        <input type="number" min="1" bind:value={atBar} />
      </label>
    </div>

    <label class="field">
      <span class="silk">melody → track</span>
      <select bind:value={targetTrackId}>
        <option value="">new midi track (auto)</option>
        {#each midiTracks as t (t.id)}
          <option value={t.id}>{t.name}</option>
        {/each}
      </select>
    </label>

    <label class="accomp" title="Chain an AMT accompaniment proposal under the melody">
      <input type="checkbox" bind:checked={accompany} />
      <span class="mono acclabel">♪ ACCOMPANY</span>
      <span class="silk">amt writes a second part under the hum</span>
    </label>

    {#if hum.phase === "error"}
      <div class="err silk" role="alert">{hum.error}</div>
    {/if}

    <button class="run mono" disabled={!!disabledReason} title={disabledReason || "Analyze the hum"} onclick={run}>
      <span class="spark">✦</span>
      {disabledReason ? disabledReason.toUpperCase() : "HUM → SONG"}
    </button>
  {:else if hum.busy}
    <!-- staged analysis -->
    <div class="jobbox live">
      <div class="wavebox"><div class="scan"></div><div class="scan lag"></div></div>
      <div class="jhead">
        <span class="jstage mono">{STAGE_LABEL[hum.stage] ?? hum.stage}</span>
        <span class="jpct mono">{Math.round(hum.progress * 100)}%</span>
      </div>
      <div class="stages">
        {#each ["loadingAudio", "trackingPitch", "segmenting", "quantizing"] as s, i (s)}
          <span
            class="stagechip mono"
            class:onstage={hum.stage === s}
            class:donestage={["loadingAudio", "trackingPitch", "segmenting", "quantizing"].indexOf(hum.stage) > i ||
              hum.phase === "applying"}
          >
            {STAGE_LABEL[s]}
          </span>
        {/each}
      </div>
      <div class="pbar"><div class="pfill" style:width="{hum.progress * 100}%"></div></div>
    </div>
    {#if hum.log.length}
      <pre class="log mono">{hum.log.slice(-3).join("\n")}</pre>
    {/if}
  {:else if hum.phase === "done"}
    <!-- landed -->
    <div class="landed">
      <div class="okline mono">▦ MELODY LANDED</div>
      <div class="okmeta silk">
        {hum.melodyNoteCount} notes{hum.createdTrackName ? ` · new track "${hum.createdTrackName}"` : ""}
      </div>
      {#if hum.accompStage === "pending"}
        <div class="accpending mono">♪ accompaniment generating… <span class="spin">◌</span></div>
      {:else if hum.accompStage === "done"}
        <div class="accdone mono">♪ accompaniment landed</div>
      {:else if hum.accompStage === "failed"}
        <div class="err silk">accompaniment failed — melody is untouched</div>
      {/if}
    </div>

    <!-- step 3: give it a voice -->
    <div class="step">
      <span class="stepno mono">3</span>
      <span class="silk">give it a voice</span>
    </div>

    {#if zyn.zynAvailable}
      <button class="zyn mono" disabled={zynBusy} onclick={bindZyn} title="Load a Zyn plucked/lead patch and bind it to the melody track">
        {zynBusy ? "◌ BINDING ZYN…" : "✦ ZYN PLUCKED / LEAD"}
      </button>
    {/if}
    <select title="Bind an instrument to the melody track" onchange={bindInstrument}>
      <option value="">bind instrument… (default polysynth)</option>
      {#each instruments.list as inst (inst.id)}
        <option value={inst.id}>◈ {inst.name}</option>
      {/each}
      {#each plugins.instances as inst (inst.id)}
        <option value={"plugin:" + inst.id}>⌁ {inst.name} ({inst.format})</option>
      {/each}
    </select>

    <button class="again mono" onclick={() => hum.reset()}>↺ HUM ANOTHER</button>
  {/if}
</div>

<style>
  .panel {
    display: flex;
    flex-direction: column;
    gap: 10px;
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding-right: 2px;
  }

  .intro {
    display: flex;
    flex-direction: column;
    gap: 2px;
  }
  .lead {
    font-size: 12px;
    letter-spacing: 0.2em;
    color: var(--magenta);
    text-shadow: 0 0 10px var(--magenta-dim);
  }
  .blurb {
    letter-spacing: 0.12em;
  }

  .step {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-top: 4px;
  }
  .stepno {
    width: 16px;
    height: 16px;
    display: grid;
    place-items: center;
    font-size: 9px;
    border-radius: 50%;
    border: 1px solid var(--cyan-dim);
    color: var(--cyan);
  }

  .recrow {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .rec {
    flex: 1;
    padding: 8px 10px;
    font-size: 10px;
    letter-spacing: 0.16em;
    border-radius: 5px;
    border: 1px solid rgba(255, 65, 82, 0.35);
    background: rgba(255, 65, 82, 0.07);
    color: rgba(255, 120, 130, 0.9);
    cursor: pointer;
    transition: box-shadow 120ms;
  }
  .rec:hover:not(:disabled) {
    box-shadow: 0 0 12px rgba(255, 65, 82, 0.25);
  }
  .rec:disabled {
    opacity: 0.4;
    cursor: not-allowed;
  }
  .rec.recording {
    color: var(--red);
    border-color: var(--red);
    animation: rec-pulse 1.2s ease-in-out infinite;
  }
  @keyframes rec-pulse {
    50% {
      box-shadow: 0 0 4px rgba(255, 65, 82, 0.1);
    }
  }
  .hint {
    color: var(--amber);
  }

  .srctabs {
    display: flex;
    gap: 6px;
  }
  .srctab {
    flex: 1;
    padding: 5px 0;
    font-size: 8px;
    letter-spacing: 0.16em;
    border-radius: 4px;
    border: 1px solid var(--glass-border);
    background: transparent;
    color: var(--text-faint);
    cursor: pointer;
  }
  .srctab.on {
    color: var(--cyan);
    border-color: var(--cyan-dim);
  }

  select,
  input[type="text"],
  input[type="number"] {
    width: 100%;
    background: rgba(5, 7, 13, 0.7);
    color: var(--text);
    border: 1px solid var(--glass-border);
    border-radius: 4px;
    font-size: 11px;
    padding: 6px 8px;
  }
  select:focus,
  input:focus {
    border-color: var(--cyan-dim);
  }

  .row {
    display: flex;
    gap: 8px;
  }
  .field {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .field.small {
    flex: 0 0 64px;
  }

  .accomp {
    display: flex;
    align-items: center;
    gap: 7px;
    cursor: pointer;
  }
  .accomp input {
    accent-color: var(--magenta);
  }
  .acclabel {
    font-size: 9px;
    letter-spacing: 0.16em;
    color: var(--magenta);
  }

  .err {
    color: var(--red);
    letter-spacing: 0.05em;
    text-transform: none;
  }

  .run,
  .again,
  .zyn {
    padding: 8px 10px;
    font-size: 10px;
    letter-spacing: 0.16em;
    border: none;
    border-radius: 5px;
    cursor: pointer;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    transition: filter 120ms;
  }
  .run {
    color: var(--bg-0);
    background: linear-gradient(100deg, var(--magenta), var(--violet));
    box-shadow:
      0 0 14px rgba(255, 79, 216, 0.3),
      0 0 22px rgba(157, 123, 255, 0.2);
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
  .zyn {
    color: var(--bg-0);
    background: linear-gradient(100deg, var(--violet), var(--cyan));
    box-shadow: 0 0 14px rgba(157, 123, 255, 0.25);
  }
  .zyn:hover:not(:disabled) {
    filter: brightness(1.15);
  }
  .zyn:disabled {
    opacity: 0.6;
    cursor: wait;
  }
  .again {
    border: 1px solid var(--glass-border);
    background: transparent;
    color: var(--text-dim);
  }
  .again:hover {
    color: var(--cyan);
    border-color: var(--cyan-dim);
  }

  /* staged analysis */
  .jobbox {
    position: relative;
    border: 1px solid var(--glass-border);
    border-radius: 6px;
    padding: 9px 11px 11px;
    background: rgba(10, 13, 23, 0.6);
    overflow: hidden;
  }
  .jobbox.live {
    border-color: color-mix(in srgb, var(--magenta) 45%, var(--violet) 25%);
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
      rgba(255, 79, 216, 0.12) 35%,
      rgba(255, 79, 216, 0.3) 50%,
      rgba(157, 123, 255, 0.2) 65%,
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
    justify-content: space-between;
    position: relative;
  }
  .jstage {
    font-size: 10px;
    color: var(--magenta);
  }
  .jpct {
    font-size: 10px;
    color: var(--text-dim);
  }
  .stages {
    position: relative;
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    margin-top: 6px;
  }
  .stagechip {
    font-size: 8px;
    letter-spacing: 0.1em;
    padding: 2px 6px;
    border-radius: 3px;
    border: 1px solid var(--glass-border);
    color: var(--text-faint);
  }
  .stagechip.onstage {
    color: var(--magenta);
    border-color: var(--magenta-dim);
    box-shadow: 0 0 8px rgba(255, 79, 216, 0.2);
  }
  .stagechip.donestage {
    color: #5cf2b8;
    border-color: rgba(92, 242, 184, 0.3);
  }
  .pbar {
    margin-top: 7px;
    height: 3px;
    border-radius: 2px;
    background: rgba(96, 130, 190, 0.15);
    overflow: hidden;
    position: relative;
  }
  .pfill {
    height: 100%;
    background: linear-gradient(90deg, var(--magenta), var(--violet));
    box-shadow: 0 0 8px rgba(255, 79, 216, 0.5);
    transition: width 160ms linear;
  }
  .log {
    margin: 0;
    padding: 5px 7px;
    font-size: 9px;
    line-height: 1.5;
    color: var(--text-dim);
    background: rgba(5, 7, 13, 0.75);
    border-radius: 4px;
    border: 1px solid rgba(96, 130, 190, 0.1);
    white-space: pre-wrap;
    word-break: break-all;
    max-height: 52px;
    overflow: hidden;
  }

  /* landed */
  .landed {
    border: 1px solid rgba(92, 242, 184, 0.3);
    border-radius: 6px;
    padding: 9px 11px;
    background: rgba(92, 242, 184, 0.05);
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .okline {
    font-size: 10px;
    letter-spacing: 0.18em;
    color: #5cf2b8;
    text-shadow: 0 0 10px rgba(92, 242, 184, 0.4);
  }
  .okmeta {
    letter-spacing: 0.1em;
  }
  .accpending {
    font-size: 9px;
    color: var(--magenta);
  }
  .accdone {
    font-size: 9px;
    color: #5cf2b8;
  }
  .spin {
    display: inline-block;
    animation: spin-pulse 0.9s linear infinite;
  }
  @keyframes spin-pulse {
    50% {
      opacity: 0.35;
    }
  }
</style>
