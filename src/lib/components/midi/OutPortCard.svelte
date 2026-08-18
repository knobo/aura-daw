<script lang="ts">
  /**
   * One open output port: its accent swatch (the colour the PATCH cords wear),
   * its clock toggle, whether the clock is running, and its counters. The
   * counters sit in fixed-width mono cells so the 500 ms poll never moves the
   * card under the pointer.
   */
  import { midiIo } from "../../state/midiio.svelte";
  import type { MidiOutputPortStatus } from "../../types/ipc";

  let {
    status,
    accent,
    live = false,
  }: {
    status: MidiOutputPortStatus;
    accent: string;
    /** Notes left this port on the last poll. */
    live?: boolean;
  } = $props();
</script>

<div class="card" style="--accent: {accent}">
  <div class="top">
    <span class="swatch" class:live aria-hidden="true"></span>
    <span class="pname mono" title={status.port.name}>{status.port.name}</span>
    <button
      class="close"
      aria-label="Close {status.port.name}"
      title="Close this port — anything patched to it stops sounding"
      onclick={() => void midiIo.closeOutPort(status.port.id)}
    >
      ×
    </button>
  </div>

  <div class="bottom">
    <label class="clock" title="Send MIDI clock + transport (Start/Stop/Continue/SPP) to this port">
      <input
        type="checkbox"
        checked={status.clockEnabled}
        onchange={(e) => void midiIo.setClockEnabled(status.port.id, e.currentTarget.checked)}
      />
      <span class="silk">clock</span>
    </label>
    <span class="run silk" class:on={status.clockEnabled && status.running}>
      {status.clockEnabled ? (status.running ? "running" : "stopped") : "no clock"}
    </span>
    <span class="counts mono">
      <span class="n">{status.notesSent}</span><span class="u silk">notes</span>
      <span class="n" class:idle={!status.clockEnabled}>{status.pulsesSent}</span><span class="u silk">pulses</span>
    </span>
  </div>
</div>

<style>
  .card {
    display: flex;
    flex-direction: column;
    gap: 5px;
    padding: 6px 8px;
    border-radius: 4px;
    border: var(--border-width) solid var(--glass-border);
    border-left: 2px solid var(--accent);
    background: rgb(var(--bg-1-rgb) / 0.55);
  }

  .top {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
  }
  .swatch {
    flex: none;
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: var(--accent);
    box-shadow: 0 0 calc(5px * var(--glow-scale)) var(--accent);
  }
  .swatch.live {
    animation: dot-pulse 1s ease-in-out infinite;
  }
  @keyframes dot-pulse {
    50% {
      opacity: 0.5;
    }
  }
  .pname {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 11px;
    color: var(--text);
  }
  .close {
    flex: none;
    border: none;
    background: transparent;
    color: var(--text-faint);
    font-size: 14px;
    line-height: 1;
    padding: 0 3px;
    cursor: pointer;
  }
  .close:hover {
    color: var(--red);
  }

  .bottom {
    display: flex;
    align-items: center;
    gap: 8px;
    min-width: 0;
  }
  .clock {
    flex: none;
    display: flex;
    align-items: center;
    gap: 4px;
    cursor: pointer;
  }
  .clock input {
    accent-color: var(--cyan);
    width: 11px;
    height: 11px;
    margin: 0;
    cursor: pointer;
  }
  .run {
    flex: none;
    color: var(--text-faint);
  }
  .run.on {
    color: var(--green);
  }

  .counts {
    margin-left: auto;
    display: flex;
    align-items: baseline;
    gap: 3px;
    font-size: 9px;
    color: var(--text-mid);
  }
  /* Reserved width: the poll changes these numbers twice a second and must
     not shove the card around. */
  .n {
    display: inline-block;
    min-width: 5ch;
    text-align: right;
  }
  .n.idle {
    color: var(--text-faint);
  }
  .u {
    letter-spacing: 0.1em;
  }

  @media (prefers-reduced-motion: reduce) {
    .swatch.live {
      animation: none;
    }
  }
</style>
