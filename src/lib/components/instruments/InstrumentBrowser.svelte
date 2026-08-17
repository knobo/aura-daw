<script lang="ts">
  /**
   * Sampler instrument browser: bank listing (sampler_list_instruments),
   * mini keyboard audition per instrument (sampler_preview_note), assignment
   * to midi tracks (set_track_instrument), and the "generate instrument"
   * bridge into AI Studio (stableAudioSfz).
   */
  import { instruments } from "../../state/instruments.svelte";
  import { project } from "../../state/project.svelte";
  import { openStudio } from "../../state/ui.svelte";
  import type { InstrumentInfo } from "../../types/ipc";

  const midiTracks = $derived(project.tracks.filter((t) => t.kind === "midi"));

  const AUDITION_KEYS = [48, 52, 55, 60, 64, 67, 72];
  const KEY_NAMES = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];
  const keyName = (k: number) => `${KEY_NAMES[k % 12]}${Math.floor(k / 12) - 1}`;

  function assignedTracks(inst: InstrumentInfo): string {
    const names = midiTracks.filter((t) => t.instrumentId === inst.id).map((t) => t.name);
    return names.join(" · ");
  }

  function onAssign(inst: InstrumentInfo, e: Event) {
    const trackId = (e.currentTarget as HTMLSelectElement).value;
    if (trackId) void instruments.assign(trackId, inst.id);
    (e.currentTarget as HTMLSelectElement).value = "";
  }
</script>

<div class="browser">
  <button class="gen mono" title="Generate a new instrument from a prompt" onclick={() => openStudio("stableAudioSfz")}>
    <span class="spark">✦</span> GENERATE INSTRUMENT
  </button>

  {#if instruments.error}
    <div class="err silk">{instruments.error}</div>
  {/if}

  {#if instruments.list.length === 0}
    <div class="empty silk">
      {instruments.loading ? "loading bank…" : "bank empty — generate one, or load an .sfz via MCP"}
    </div>
  {/if}

  <div class="list">
    {#each instruments.list as inst (inst.id)}
      <div class="inst" class:previewing={instruments.previewingId === inst.id}>
        <div class="row top">
          <span class="iname">{inst.name}</span>
          <span class="regions silk">{inst.regionCount} rgn</span>
        </div>
        <div class="row range silk">
          {#if inst.keyLow != null && inst.keyHigh != null}
            {keyName(inst.keyLow)} – {keyName(inst.keyHigh)}
          {/if}
          <span class="path mono" title={inst.sfzPath}>{inst.sfzPath.split("/").pop()}</span>
        </div>

        <div class="row audition" role="group" aria-label="Audition {inst.name}">
          {#each AUDITION_KEYS as k (k)}
            <button
              class="pkey mono"
              class:black={[1, 3, 6, 8, 10].includes(k % 12)}
              title="Preview {keyName(k)}"
              onclick={() => instruments.preview(inst.id, k)}
            >
              {keyName(k)}
            </button>
          {/each}
        </div>

        <div class="row bind">
          <select title="Bind to a MIDI track" onchange={(e) => onAssign(inst, e)}>
            <option value="">assign to track…</option>
            {#each midiTracks as t (t.id)}
              <option value={t.id}>{t.name}{t.instrumentId === inst.id ? " ✓" : ""}</option>
            {/each}
          </select>
          {#if assignedTracks(inst)}
            <span class="bound silk" title="Bound tracks">⌁ {assignedTracks(inst)}</span>
          {/if}
        </div>
      </div>
    {/each}
  </div>

  {#if midiTracks.length === 0}
    <div class="hint silk">no midi tracks yet — add one with “+ midi” in the track rail</div>
  {/if}
</div>

<style>
  .browser {
    display: flex;
    flex-direction: column;
    gap: 10px;
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding-right: 2px;
  }

  .gen {
    padding: 8px 10px;
    font-size: 10px;
    letter-spacing: 0.16em;
    border: none;
    border-radius: 5px;
    cursor: pointer;
    color: var(--bg-0);
    background: linear-gradient(100deg, var(--cyan), var(--violet));
    box-shadow: 0 0 calc(14px * var(--glow-scale)) rgb(var(--cyan-rgb) / 0.25);
    transition: filter 120ms;
  }
  .gen:hover {
    filter: brightness(1.15);
  }
  .spark {
    margin-right: 4px;
  }

  .err {
    color: var(--red);
  }
  .empty,
  .hint {
    padding: 8px 0;
  }

  .list {
    display: flex;
    flex-direction: column;
    gap: 8px;
  }
  .inst {
    border: var(--border-width) solid var(--glass-border);
    border-radius: 6px;
    background: rgb(var(--bg-1-rgb) / 0.6);
    padding: 8px 10px;
    display: flex;
    flex-direction: column;
    gap: 6px;
    transition: border-color 150ms, box-shadow 150ms;
  }
  .inst.previewing {
    border-color: var(--cyan-dim);
    box-shadow: 0 0 calc(12px * var(--glow-scale)) rgb(var(--cyan-rgb) / 0.2);
  }

  .row {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .iname {
    flex: 1;
    font-size: 12px;
    font-weight: 500;
    color: var(--text);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .regions {
    color: var(--text-faint);
  }
  .range {
    justify-content: space-between;
  }
  .path {
    font-size: 9px;
    color: var(--text-faint);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 60%;
  }

  .audition {
    gap: 3px;
  }
  .pkey {
    flex: 1;
    padding: 5px 0;
    font-size: 8px;
    letter-spacing: 0.05em;
    border-radius: 3px;
    border: var(--border-width) solid rgb(var(--edge-rgb) / 0.18);
    background: rgb(var(--bg-3-rgb) / 0.6);
    color: var(--text-dim);
    cursor: pointer;
    transition: background 80ms, color 80ms;
  }
  .pkey.black {
    background: rgb(var(--bg-0-rgb) / 0.8);
  }
  .pkey:hover {
    color: var(--bg-0);
    background: var(--cyan);
    border-color: var(--cyan);
  }
  .pkey:active {
    filter: brightness(1.3);
  }

  .bind select {
    flex: 1;
    min-width: 0;
    background: rgb(var(--bg-0-rgb) / 0.7);
    color: var(--text-dim);
    border: var(--border-width) solid var(--glass-border);
    border-radius: 4px;
    font-size: 10px;
    padding: 4px 6px;
  }
  .bound {
    color: var(--green);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 45%;
  }
</style>
