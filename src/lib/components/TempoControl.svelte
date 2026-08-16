<script lang="ts">
  /**
   * Transport-bar tempo + meter editor. Click the readout to type a BPM
   * or drag the strip; chips set a single project-wide time signature.
   * Writes go through midi.setTempo / setMeter (one TempoSet each).
   */
  import { project } from "../state/project.svelte";
  import { midi } from "../state/midi.svelte";
  import { COMMON_METERS, TEMPO_MAX, TEMPO_MIN, nudgeTempo, parseTempo } from "../utils/tempo";

  let open = $state(false);
  let draft = $state("");
  let inputEl: HTMLInputElement | undefined = $state();
  let wheelEnd: ReturnType<typeof setTimeout> | null = null;

  const fill = $derived(
    (project.tempoBpm - TEMPO_MIN) / (TEMPO_MAX - TEMPO_MIN),
  );

  function openEditor() {
    draft = project.tempoBpm.toFixed(1);
    open = true;
  }

  let token: Promise<string | undefined> | undefined;
  function openGesture(label: string) {
    token = project.beginGesture(label);
  }
  function closeGesture() {
    const idp = token;
    token = undefined;
    void idp?.then((id) => project.endGesture(id));
  }

  function close() {
    open = false;
    closeGesture();
  }

  function commitDraft() {
    const parsed = parseTempo(draft);
    if (parsed != null) void midi.setTempo(parsed);
    close();
  }

  function onToggle() {
    if (open) commitDraft();
    else openEditor();
  }

  function onWheel(e: WheelEvent) {
    e.preventDefault();
    if (!wheelEnd) openGesture("set tempo");
    else clearTimeout(wheelEnd);
    const wheelToken = token;
    const next = nudgeTempo(project.tempoBpm, e.deltaY, e.shiftKey);
    if (open) draft = next.toFixed(1);
    void midi.setTempo(next);
    wheelEnd = setTimeout(() => {
      wheelEnd = null;
      void wheelToken?.then((id) => project.endGesture(id));
    }, 250);
  }

  function onSliderDown(e: PointerEvent) {
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    openGesture("set tempo");
  }

  function onSliderInput(e: Event) {
    const v = Number((e.currentTarget as HTMLInputElement).value);
    draft = v.toFixed(1);
    void midi.setTempo(v);
  }

  function onSliderUp() {
    closeGesture();
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Enter") {
      e.preventDefault();
      commitDraft();
    } else if (e.key === "Escape") {
      e.preventDefault();
      close();
    }
  }

  $effect(() => {
    if (open && inputEl) {
      inputEl.focus();
      inputEl.select();
    }
  });
</script>

<div class="tempo-wrap">
  <button
    class="tempo mono"
    class:open
    title="Click to set tempo and time signature. Scroll to nudge BPM."
    aria-haspopup="dialog"
    aria-expanded={open}
    onclick={onToggle}
    onwheel={onWheel}
  >
    <span class="silk">tempo</span>
    <span class="val">{project.tempoBpm.toFixed(1)}</span>
    <span class="unit silk">bpm · {project.timeSignature[0]}/{project.timeSignature[1]}</span>
  </button>

  {#if open}
    <div class="backdrop" role="presentation" onpointerdown={close}></div>
    <div
      class="panel glass mono"
      role="dialog"
      tabindex="-1"
      aria-label="Tempo and time signature"
      onkeydown={onKeydown}
    >
      <label class="field">
        <span class="silk">bpm</span>
        <input
          bind:this={inputEl}
          class="bpm"
          type="text"
          inputmode="decimal"
          spellcheck="false"
          bind:value={draft}
          onblur={() => {
            if (!open) return;
            const parsed = parseTempo(draft);
            if (parsed != null) void midi.setTempo(parsed);
          }}
        />
      </label>

      <div class="strip">
        <input
          class="slider"
          type="range"
          min={TEMPO_MIN}
          max={TEMPO_MAX}
          step="0.1"
          value={project.tempoBpm}
          style:--fill="{Math.min(1, Math.max(0, fill))}"
          aria-label="Tempo"
          onpointerdown={onSliderDown}
          onpointerup={onSliderUp}
          onpointercancel={onSliderUp}
          oninput={onSliderInput}
        />
        <div class="range silk">
          <span>{TEMPO_MIN}</span>
          <span>{TEMPO_MAX}</span>
        </div>
      </div>

      <div class="meters">
        <span class="silk">meter</span>
        <div class="chips">
          {#each COMMON_METERS as [num, den] (num + "/" + den)}
            <button
              class="mchip"
              class:on={project.timeSignature[0] === num && project.timeSignature[1] === den}
              onclick={() => void midi.setMeter(num, den)}
            >
              {num}/{den}
            </button>
          {/each}
        </div>
      </div>
    </div>
  {/if}
</div>

<style>
  .tempo-wrap {
    position: relative;
    z-index: 2;
  }
  .tempo {
    display: flex;
    flex-direction: column;
    line-height: 1.25;
    align-items: flex-start;
    background: transparent;
    border: 1px solid transparent;
    border-radius: 5px;
    padding: 2px 6px;
    margin: -2px -6px;
    cursor: pointer;
    text-align: left;
    color: inherit;
    position: relative;
    z-index: 91;
  }
  .tempo:hover,
  .tempo.open {
    background: rgba(82, 229, 255, 0.07);
    border-color: var(--glass-border);
  }
  .tempo .val {
    font-size: 15px;
    color: var(--text);
  }
  .tempo.open .val {
    color: var(--cyan);
    text-shadow: 0 0 10px var(--cyan-dim);
  }

  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 89;
  }
  .panel {
    position: absolute;
    top: calc(100% + 8px);
    left: 0;
    z-index: 90;
    width: 228px;
    padding: 10px 12px 12px;
    display: flex;
    flex-direction: column;
    gap: 10px;
    border: 1px solid var(--glass-border);
    border-radius: 6px;
  }

  .field {
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  .bpm {
    flex: 1;
    min-width: 0;
    font: inherit;
    font-size: 18px;
    letter-spacing: 0.04em;
    color: var(--cyan);
    background: rgba(5, 7, 13, 0.75);
    border: 1px solid var(--cyan-dim);
    border-radius: 4px;
    padding: 2px 8px;
    outline: none;
    box-shadow: 0 0 12px rgba(82, 229, 255, 0.18);
    user-select: text;
    -webkit-user-select: text;
  }
  .bpm:focus {
    border-color: var(--cyan);
  }

  .strip {
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .slider {
    -webkit-appearance: none;
    appearance: none;
    width: 100%;
    height: 8px;
    border-radius: 2px;
    background: linear-gradient(
      90deg,
      var(--cyan) 0%,
      var(--cyan) calc(var(--fill) * 100%),
      rgba(16, 20, 42, 0.9) calc(var(--fill) * 100%),
      rgba(16, 20, 42, 0.9) 100%
    );
    border: 1px solid var(--glass-border);
    cursor: ew-resize;
  }
  .slider::-webkit-slider-thumb {
    -webkit-appearance: none;
    appearance: none;
    width: 10px;
    height: 14px;
    border-radius: 1px;
    background: var(--text);
    border: 1px solid var(--cyan);
    box-shadow: 0 0 8px var(--cyan-dim);
    cursor: ew-resize;
  }
  .range {
    display: flex;
    justify-content: space-between;
    color: var(--text-faint);
  }

  .meters {
    display: flex;
    flex-direction: column;
    gap: 5px;
  }
  .chips {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
  }
  .mchip {
    font: inherit;
    font-size: 9px;
    letter-spacing: 0.08em;
    padding: 3px 6px;
    border-radius: 3px;
    border: 1px solid var(--glass-border);
    background: transparent;
    color: var(--text-faint);
    cursor: pointer;
  }
  .mchip.on,
  .mchip:hover {
    color: var(--cyan);
    border-color: var(--cyan-dim);
  }
</style>
