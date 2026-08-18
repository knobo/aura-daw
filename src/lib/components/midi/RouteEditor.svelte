<script lang="ts">
  /**
   * The inline patch editor: where a route's port, channel and audio return
   * are changed, right under the row it belongs to. Track scope gets the
   * return device (the input the external synth comes back on); clip scope
   * does not — the backend never stores one for a clip.
   *
   * Presentational only: every change leaves through a callback so the
   * caller decides whether it is a track route or a clip override.
   */
  import type { AudioDevice, MidiPortInfo } from "../../types/ipc";
  import type { RouteTarget } from "./routing-view";

  let {
    target,
    openPorts,
    closedPorts,
    returns = null,
    returnDevice = null,
    scopeLabel,
    onport,
    onchannel,
    onreturn,
    onclear,
  }: {
    target: RouteTarget | null;
    /** Ports that are open right now — notes flow the moment you pick one. */
    openPorts: MidiPortInfo[];
    /** Present on the machine but nobody opened them. */
    closedPorts: MidiPortInfo[];
    /** Audio inputs for the return field; null hides the field (clip scope). */
    returns?: AudioDevice[] | null;
    returnDevice?: string | null;
    /** Named in the labels so a nested clip editor is never mistaken for its track's. */
    scopeLabel: string;
    onport: (portId: string | null) => void;
    /** `null` = leave every note on the channel it has in the clip. */
    onchannel: (channel: number | null) => void;
    onreturn?: (deviceId: string | null) => void;
    onclear: () => void;
  } = $props();

  /** The picker's value, "" for "from clip"; 0-15 on the wire, 1-16 shown. */
  function toChannel(raw: string): number | null {
    if (raw === "") return null;
    const n = Number(raw);
    if (!Number.isFinite(n)) return null;
    return Math.max(0, Math.min(15, Math.round(n)));
  }

  const CHANNELS = Array.from({ length: 16 }, (_, i) => i);

  const missingPort = $derived(target && target.health === "missing" ? target : null);
</script>

<div class="editor">
  <label class="field">
    <span class="silk">port</span>
    <select
      aria-label="Output port for {scopeLabel}"
      onchange={(e) => onport(e.currentTarget.value || null)}
    >
      <option value="" selected={!target}>nowhere — notes stay inside AURA</option>
      {#each openPorts as p (p.id)}
        <option value={p.id} selected={p.id === target?.portId}>{p.name}</option>
      {/each}
      {#if closedPorts.length > 0}
        <optgroup label="not open — open it in OUT first">
          {#each closedPorts as p (p.id)}
            <option value={p.id} selected={p.id === target?.portId}>{p.name}</option>
          {/each}
        </optgroup>
      {/if}
      {#if missingPort}
        <optgroup label="missing">
          <option value={missingPort.portId} selected>{missingPort.portName}</option>
        </optgroup>
      {/if}
    </select>
  </label>

  <div class="pair">
    <label class="field chan">
      <span class="silk">channel</span>
      <select
        class="mono"
        disabled={!target}
        aria-label="MIDI channel for {scopeLabel}"
        title="Leave it on 'from clip' unless the device needs one fixed channel — General MIDI drums live on channel 10, and that is what the clip already says"
        onchange={(e) => onchannel(toChannel(e.currentTarget.value))}
      >
        <option value="" selected={target?.channel == null}>from clip</option>
        {#each CHANNELS as ch (ch)}
          <option value={String(ch)} selected={target?.channel === ch}>{ch + 1}</option>
        {/each}
      </select>
    </label>
    <button
      class="clear mono"
      disabled={!target}
      title="Unpatch {scopeLabel} — its notes stay inside AURA"
      onclick={onclear}
    >
      Clear route
    </button>
  </div>

  {#if returns}
    <label class="field">
      <span class="silk">return</span>
      <select
        disabled={!target}
        aria-label="Audio return device for {scopeLabel}"
        title={target ? undefined : "Pick an output port first — a return needs a route to hang on"}
        onchange={(e) => onreturn?.(e.currentTarget.value || null)}
      >
        <option value="" selected={!returnDevice}>none — MIDI only</option>
        {#each returns as d (d.id)}
          <option value={d.id} selected={d.id === returnDevice}>{d.name}</option>
        {/each}
      </select>
    </label>
    <p class="hint silk">the audio input the outboard synth comes back on — patch the cable yourself</p>
  {/if}
</div>

<style>
  .editor {
    display: flex;
    flex-direction: column;
    gap: 5px;
    margin: 4px 0 6px 21px;
    padding: 7px 8px;
    border-radius: 4px;
    border: var(--border-width) solid var(--glass-border);
    border-left: 2px solid var(--cyan-dim);
    background: rgb(var(--bg-1-rgb) / 0.5);
  }

  .field {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
  }
  .field .silk {
    flex: none;
    width: 46px;
  }
  .pair {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .chan {
    flex: none;
  }
  .chan .silk {
    width: 46px;
  }

  select {
    flex: 1;
    min-width: 0;
    background: rgb(var(--bg-2-rgb) / 0.8);
    color: var(--text);
    border: var(--border-width) solid var(--glass-border);
    border-radius: 2px;
    font-size: 10px;
    font-family: var(--font-ui);
    padding: 3px 4px;
  }
  select:disabled {
    color: var(--text-faint);
  }
  .chan select {
    flex: none;
    width: 76px;
    font-family: var(--font-mono);
  }

  .clear {
    margin-left: auto;
    border: var(--border-width) solid var(--glass-border);
    background: transparent;
    color: var(--text-mid);
    border-radius: 2px;
    padding: 3px 7px;
    font-size: 9px;
    letter-spacing: 0.08em;
    cursor: pointer;
  }
  .clear:hover:not(:disabled) {
    color: var(--red-soft);
    border-color: rgb(var(--red-rgb) / 0.4);
  }
  .clear:disabled {
    color: var(--text-faint);
    cursor: default;
  }

  .hint {
    margin: 0;
    letter-spacing: 0.08em;
    line-height: 1.5;
    text-transform: none;
    color: var(--text-dim);
  }
</style>
