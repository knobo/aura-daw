<script lang="ts">
  /**
   * Export ("bounce") dialog: formats from export_capabilities (ffmpeg-gated
   * ones greyed with a hint), destination defaulting into the project dir,
   * wav bit depth, full/loop region, optional sample rate, master gain and
   * tail — then export_song with a live progress bar from export://progress
   * and a success toast (path · duration · size) from export://done.
   */
  import { exporter } from "../../state/exporter.svelte";
  import { project } from "../../state/project.svelte";
  import { transport } from "../../state/transport.svelte";

  const ALL_FORMATS = ["wav", "mp3", "flac", "ogg", "m4a"];

  let path = $state("");
  let format = $state("wav");
  let bitDepth = $state(24);
  let region = $state<"full" | "loop">("full");
  let sampleRate = $state(""); // "" = engine rate
  let masterGainDb = $state(0);
  let tailSeconds = $state<number | null>(null); // null = per-region default
  let pathEdited = $state(false);

  const caps = $derived(exporter.caps);
  const hasLoop = $derived(
    transport.snap.loopEndSamples > transport.snap.loopStartSamples,
  );
  const job = $derived(exporter.job);

  function available(f: string): boolean {
    return caps?.formats.includes(f) ?? f === "wav";
  }

  // (Re)seed the destination whenever the dialog opens fresh.
  let seededForOpen = $state(false);
  $effect(() => {
    if (exporter.open && !seededForOpen) {
      seededForOpen = true;
      pathEdited = false;
      path = exporter.defaultPath(format);
      if (region === "loop" && !hasLoop) region = "full";
    } else if (!exporter.open && seededForOpen) {
      seededForOpen = false;
    }
  });

  function pickFormat(f: string) {
    if (!available(f) || exporter.busy) return;
    format = f;
    if (!pathEdited) {
      path = exporter.defaultPath(f);
    } else {
      // keep a hand-edited path, just swap the extension
      path = path.replace(/\.[A-Za-z0-9]+$/, "") + `.${f}`;
    }
  }

  const defaultTail = $derived(region === "loop" ? 0 : 1);

  async function start() {
    const rate = parseInt(sampleRate, 10);
    await exporter.start({
      path,
      format,
      region,
      bitDepth: format === "wav" ? bitDepth : null,
      sampleRate: Number.isFinite(rate) ? rate : null,
      masterGainDb,
      tailSeconds,
    });
  }

  function onOverlayKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") exporter.closeDialog();
  }
</script>

{#if exporter.open}
  <div
    class="overlay"
    role="presentation"
    onkeydown={onOverlayKeydown}
    onpointerdown={(e) => {
      if (e.target === e.currentTarget && !exporter.busy) exporter.closeDialog();
    }}
  >
    <div class="dialog glass" role="dialog" aria-modal="true" aria-label="Export song">
      <div class="head">
        <span class="title mono">⤓ EXPORT</span>
        <span class="sub silk">offline bounce · faster than realtime</span>
        <button class="x mono" title="Close" onclick={() => exporter.closeDialog()}>×</button>
      </div>

      <label class="field">
        <span class="silk">destination</span>
        <input
          type="text"
          class="mono pathin"
          bind:value={path}
          oninput={() => (pathEdited = true)}
          disabled={exporter.busy}
          spellcheck="false"
        />
      </label>

      <div class="field">
        <span class="silk">format</span>
        <div class="formats">
          {#each ALL_FORMATS as f (f)}
            <button
              class="fmt mono"
              class:on={format === f}
              class:off={!available(f)}
              disabled={!available(f) || exporter.busy}
              title={available(f) ? `Export ${f}` : (caps?.hint ?? "requires ffmpeg")}
              onclick={() => pickFormat(f)}
            >
              {f.toUpperCase()}
            </button>
          {/each}
        </div>
        {#if caps && !caps.ffmpeg}
          <div class="hint silk">⚠ {caps.hint ?? "install ffmpeg to export mp3/flac/ogg/m4a"}</div>
        {/if}
      </div>

      <div class="row">
        {#if format === "wav"}
          <label class="field small">
            <span class="silk">bit depth</span>
            <select bind:value={bitDepth} disabled={exporter.busy}>
              <option value={16}>16-bit</option>
              <option value={24}>24-bit</option>
              <option value={32}>32-bit float</option>
            </select>
          </label>
        {/if}
        <label class="field small">
          <span class="silk">region</span>
          <select bind:value={region} disabled={exporter.busy}>
            <option value="full">full song</option>
            <option value="loop" disabled={!hasLoop}>
              loop region{hasLoop ? "" : " (none set)"}
            </option>
          </select>
        </label>
        <label class="field small">
          <span class="silk">sample rate</span>
          <select bind:value={sampleRate} disabled={exporter.busy}>
            <option value="">engine ({project.sampleRate} Hz)</option>
            <option value="44100">44100 Hz</option>
            <option value="48000">48000 Hz</option>
            <option value="96000">96000 Hz</option>
          </select>
        </label>
      </div>

      <div class="row">
        <label class="field small">
          <span class="silk">master gain dB</span>
          <input type="number" min="-24" max="24" step="0.5" bind:value={masterGainDb} disabled={exporter.busy} />
        </label>
        <label class="field small">
          <span class="silk">tail s</span>
          <input
            type="number"
            min="0"
            max="60"
            step="0.5"
            placeholder={String(defaultTail)}
            value={tailSeconds ?? ""}
            oninput={(e) => {
              const v = parseFloat((e.currentTarget as HTMLInputElement).value);
              tailSeconds = Number.isFinite(v) ? v : null;
            }}
            disabled={exporter.busy}
          />
        </label>
      </div>

      {#if exporter.submitError}
        <div class="err silk" role="alert">{exporter.submitError}</div>
      {/if}

      {#if job}
        <div class="jobbox" class:live={job.state === "running"} class:errb={job.state === "error"}>
          {#if job.state === "running"}
            <div class="wavebox"><div class="scan"></div><div class="scan lag"></div></div>
          {/if}
          <div class="jhead">
            <span class="jstage mono">{job.stage}</span>
            <span class="jpct mono">{Math.round(job.progress * 100)}%</span>
          </div>
          <div class="pbar"><div class="pfill" style:width="{job.progress * 100}%"></div></div>
          {#if job.state === "done" && job.result}
            <div class="result mono">
              ↳ {job.result.path}
              <span class="meta">
                {job.result.durationSeconds.toFixed(2)} s · {(job.result.sizeBytes / (1024 * 1024)).toFixed(2)} MB
              </span>
            </div>
          {:else if job.state === "error"}
            <div class="result errtext">{job.error}</div>
          {/if}
        </div>
      {/if}

      <button class="run mono" disabled={exporter.busy || !path} onclick={start}>
        {exporter.busy ? "◌ BOUNCING…" : "⤓ EXPORT SONG"}
      </button>
    </div>
  </div>
{/if}

<style>
  .overlay {
    position: fixed;
    inset: 0;
    z-index: 80;
    display: grid;
    place-items: center;
    background: rgb(var(--scrim-rgb) / 0.6);
    backdrop-filter: blur(var(--glass-blur));
  }
  .dialog {
    width: 440px;
    max-width: calc(100vw - 40px);
    max-height: calc(100vh - 60px);
    overflow-y: auto;
    border-radius: 9px;
    padding: 14px 16px 16px;
    display: flex;
    flex-direction: column;
    gap: 11px;
    background: rgb(var(--bg-sunken-rgb) / 0.94);
    animation: dialog-in 160ms ease-out;
  }
  @keyframes dialog-in {
    from {
      transform: translateY(8px);
      opacity: 0;
    }
  }

  .head {
    display: flex;
    align-items: baseline;
    gap: 10px;
  }
  .title {
    font-size: 12px;
    letter-spacing: 0.22em;
    color: var(--cyan);
    text-shadow: 0 0 10px var(--cyan-dim);
  }
  .sub {
    flex: 1;
  }
  .x {
    border: none;
    background: transparent;
    color: var(--text-faint);
    cursor: pointer;
    font-size: 14px;
  }
  .x:hover {
    color: var(--red);
  }

  .field {
    display: flex;
    flex-direction: column;
    gap: 4px;
    min-width: 0;
  }
  .field.small {
    flex: 1;
  }
  .row {
    display: flex;
    gap: 9px;
  }
  input[type="text"],
  input[type="number"],
  select {
    background: rgb(var(--bg-0-rgb) / 0.7);
    color: var(--text);
    border: var(--border-width) solid var(--glass-border);
    border-radius: 4px;
    font-size: 11px;
    padding: 6px 8px;
  }
  input:focus,
  select:focus {
    border-color: var(--cyan-dim);
  }
  .pathin {
    font-size: 10px;
  }

  .formats {
    display: flex;
    gap: 6px;
  }
  .fmt {
    flex: 1;
    padding: 6px 0;
    font-size: 9px;
    letter-spacing: 0.14em;
    border-radius: 4px;
    border: var(--border-width) solid var(--glass-border);
    background: rgb(var(--bg-2-rgb) / 0.4);
    color: var(--text-dim);
    cursor: pointer;
    transition: border-color 120ms, color 120ms, box-shadow 120ms;
  }
  .fmt.on {
    color: var(--cyan);
    border-color: var(--cyan-dim);
    box-shadow: 0 0 var(--glow-blur) rgb(var(--cyan-rgb) / 0.18);
  }
  .fmt.off {
    opacity: 0.32;
    cursor: not-allowed;
    text-decoration: line-through;
  }
  .hint {
    color: var(--amber);
    letter-spacing: 0.1em;
  }
  .err {
    color: var(--red);
    letter-spacing: 0.05em;
    text-transform: none;
  }

  .jobbox {
    position: relative;
    border: var(--border-width) solid var(--glass-border);
    border-radius: 6px;
    padding: 8px 10px;
    background: rgb(var(--bg-1-rgb) / 0.6);
    overflow: hidden;
  }
  .jobbox.live {
    border-color: color-mix(in srgb, var(--cyan) 45%, var(--magenta) 25%);
  }
  .jobbox.errb {
    border-color: rgb(var(--red-rgb) / 0.4);
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
      rgb(var(--cyan-rgb) / 0.12) 35%,
      rgb(var(--cyan-rgb) / 0.3) 50%,
      rgb(var(--magenta-rgb) / 0.2) 65%,
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
    justify-content: space-between;
    position: relative;
  }
  .jstage {
    font-size: 10px;
    color: var(--cyan);
  }
  .jpct {
    font-size: 10px;
    color: var(--text-dim);
  }
  .pbar {
    margin-top: 5px;
    height: 3px;
    border-radius: 2px;
    background: rgb(var(--line-rgb) / 0.15);
    overflow: hidden;
    position: relative;
  }
  .pfill {
    height: 100%;
    background: linear-gradient(90deg, var(--cyan), var(--magenta));
    box-shadow: 0 0 var(--glow-blur) rgb(var(--cyan-rgb) / 0.5);
    transition: width 160ms linear;
  }
  .result {
    position: relative;
    margin-top: 6px;
    font-size: 9px;
    color: var(--green);
    word-break: break-all;
  }
  .result .meta {
    color: var(--text-dim);
    margin-left: 6px;
  }
  .result.errtext {
    color: var(--red);
    font-family: var(--font-ui);
    font-size: 10px;
  }

  .run {
    padding: 9px 10px;
    font-size: 10px;
    letter-spacing: 0.18em;
    border: none;
    border-radius: 5px;
    cursor: pointer;
    color: var(--bg-0);
    background: linear-gradient(100deg, var(--cyan), var(--magenta));
    box-shadow:
      0 0 14px rgb(var(--cyan-rgb) / 0.3),
      0 0 22px rgb(var(--magenta-rgb) / 0.2);
    transition: filter 120ms;
  }
  .run:hover:not(:disabled) {
    filter: brightness(1.15);
  }
  .run:disabled {
    filter: saturate(0.15) brightness(0.7);
    cursor: not-allowed;
  }
</style>
