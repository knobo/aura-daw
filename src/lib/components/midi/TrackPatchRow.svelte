<script lang="ts">
  /**
   * One track's patch: an IN gate on the left edge, the track, a cord tinted
   * from the track's colour into the port's accent, and the OUT jack on the
   * right edge. The row IS the signal path — read left to right and you have
   * read the routing.
   *
   * Clip overrides hang under the track, indented and quieter, because that
   * is their standing: a clip route beats its track's.
   */
  import { midiIo } from "../../state/midiio.svelte";
  import type { AudioDevice, MidiPortInfo } from "../../types/ipc";
  import type { ClipRouteRow, TrackRoutingRow } from "./routing-view";
  import PortJack from "./PortJack.svelte";
  import RouteEditor from "./RouteEditor.svelte";

  let {
    row,
    openPorts,
    closedPorts,
    returns,
    accent,
    flowing,
    capturing,
  }: {
    row: TrackRoutingRow;
    openPorts: MidiPortInfo[];
    closedPorts: MidiPortInfo[];
    returns: AudioDevice[];
    /** Port id → resolved CSS colour for its accent slot. */
    accent: (portId: string | null | undefined) => string;
    /** Port id → notes left it on the last poll. */
    flowing: (portId: string | null | undefined) => boolean;
    /** The input port is capturing right now. */
    capturing: boolean;
  } = $props();

  let editing = $state(false);
  let editingClip = $state<string | null>(null);

  const trouble = $derived(
    row.target && row.target.health !== "open" ? row.target.health : null,
  );

  function jackLabel(
    name: string,
    r: { portName: string; channel: number | null; health: string } | null,
  ): string {
    if (!r) return `${name} is not patched — pick a port`;
    const note =
      r.health === "closed"
        ? ", port not open"
        : r.health === "missing"
          ? ", device missing"
          : "";
    const chan = r.channel === null ? "channel from clip" : `channel ${r.channel + 1}`;
    return `${name} → ${r.portName}, ${chan}${note} — edit this patch`;
  }

  function toggleGate() {
    void midiIo.setInputTrack(row.isInputTarget ? null : row.trackId);
  }

  function setTrackPort(portId: string | null) {
    void midiIo.setTrackRoute(row.trackId, portId, row.target?.channel ?? null);
  }
  function setTrackChannel(channel: number | null) {
    if (!row.target) return;
    void midiIo.setTrackRoute(row.trackId, row.target.portId, channel);
  }
  function setClipPort(clip: ClipRouteRow, portId: string | null) {
    void midiIo.setClipRoute(clip.clipId, portId, clip.target.channel);
  }
  function setClipChannel(clip: ClipRouteRow, channel: number | null) {
    void midiIo.setClipRoute(clip.clipId, clip.target.portId, channel);
  }
</script>

<div class="patch" class:intarget={row.isInputTarget}>
  <div class="cordrow">
    <button
      class="gate"
      class:on={row.isInputTarget}
      aria-pressed={row.isInputTarget}
      aria-label={row.isInputTarget
        ? `Stop sending MIDI input to ${row.trackName}`
        : `Send MIDI input to ${row.trackName}`}
      title={row.isInputTarget
        ? "Incoming notes land on this track — click to stop"
        : "Send incoming MIDI notes to this track (only one track at a time)"}
      onclick={toggleGate}
    >
      <span class="gatemark" class:pulse={row.isInputTarget && capturing}></span>
    </button>

    <span class="track" style="--tint: {row.trackColor}">
      <span class="tname" title={row.trackName}>{row.trackName}</span>
    </span>

    <span
      class="cord"
      class:dead={!row.target}
      class:flow={flowing(row.target?.portId)}
      class:closed={trouble === "closed"}
      class:missing={trouble === "missing"}
      style="--accent: {accent(row.target?.portId)}; --tint: {row.trackColor}"
      aria-hidden="true"
    ></span>

    <button
      class="jackbtn"
      aria-expanded={editing}
      aria-label={jackLabel(row.trackName, row.target)}
      title="Edit this patch — port, channel, audio return"
      onclick={() => (editing = !editing)}
    >
      <PortJack
        target={row.target}
        accent={accent(row.target?.portId)}
        live={flowing(row.target?.portId)}
      />
    </button>
  </div>

  {#if trouble === "closed" && row.target}
    <p class="trouble amber">
      Port is not open — notes are not leaving AURA.
      <button class="inline mono" onclick={() => void midiIo.openOutPort(row.target?.portId ?? "")}>
        Open port
      </button>
    </p>
  {:else if trouble === "missing"}
    <p class="trouble red">Device missing — reconnect it or pick another port.</p>
  {/if}

  {#if editing}
    <RouteEditor
      target={row.target}
      {openPorts}
      {closedPorts}
      {returns}
      returnDevice={row.returnDevice}
      scopeLabel={row.trackName}
      onport={setTrackPort}
      onchannel={setTrackChannel}
      onreturn={(id) => void midiIo.setTrackReturn(row.trackId, id)}
      onclear={() => setTrackPort(null)}
    />
  {/if}

  {#if row.overrides.length > 0}
    <div class="overs">
      <span class="oheader silk">clip routes — these beat the track</span>
      {#each row.overrides as clip (clip.clipId)}
        <div class="over">
          <span class="branch" aria-hidden="true"></span>
          <span class="clip" title={clip.clipName}>{clip.clipName}</span>
          <span
            class="cord thin"
            class:closed={clip.target.health === "closed"}
            class:missing={clip.target.health === "missing"}
            class:flow={flowing(clip.target.portId)}
            style="--accent: {accent(clip.target.portId)}; --tint: {row.trackColor}"
            aria-hidden="true"
          ></span>
          <button
            class="jackbtn"
            aria-expanded={editingClip === clip.clipId}
            aria-label={jackLabel(`Clip ${clip.clipName}`, clip.target)}
            title="Edit this clip's patch"
            onclick={() => (editingClip = editingClip === clip.clipId ? null : clip.clipId)}
          >
            <PortJack
              target={clip.target}
              accent={accent(clip.target.portId)}
              live={flowing(clip.target.portId)}
            />
          </button>
        </div>
        {#if clip.target.health !== "open"}
          <p class="trouble small" class:amber={clip.target.health === "closed"} class:red={clip.target.health === "missing"}>
            {clip.target.health === "closed"
              ? "Port is not open — this clip's notes are not leaving AURA."
              : "Device missing — reconnect it or pick another port."}
          </p>
        {/if}
        {#if editingClip === clip.clipId}
          <RouteEditor
            target={clip.target}
            {openPorts}
            {closedPorts}
            scopeLabel="clip {clip.clipName}"
            onport={(portId) => setClipPort(clip, portId)}
            onchannel={(channel) => setClipChannel(clip, channel)}
            onclear={() => setClipPort(clip, null)}
          />
        {/if}
      {/each}
    </div>
  {/if}
</div>

<style>
  .patch {
    display: flex;
    flex-direction: column;
    padding: 2px 0;
    border-bottom: var(--border-width) solid rgb(var(--line-rgb) / 0.06);
  }

  .cordrow {
    display: flex;
    align-items: center;
    gap: 0;
    min-width: 0;
  }

  /* IN gate — a socket on the row's left edge. Lit means incoming notes land
     on this track; it pulses while the input port is capturing. */
  .gate {
    flex: none;
    width: 15px;
    height: 20px;
    display: grid;
    place-items: center;
    padding: 0;
    border: none;
    background: transparent;
    cursor: pointer;
  }
  .gatemark {
    width: 7px;
    height: 7px;
    border-radius: 2px;
    border: 1px solid rgb(var(--line-rgb) / 0.4);
    transition: background 90ms linear, box-shadow 90ms linear;
  }
  .gate:hover .gatemark {
    border-color: var(--cyan-dim);
  }
  .gate.on .gatemark {
    background: var(--cyan);
    border-color: var(--cyan);
    box-shadow: 0 0 calc(5px * var(--glow-scale)) var(--cyan);
  }
  .gatemark.pulse {
    animation: dot-pulse 1s ease-in-out infinite;
  }
  @keyframes dot-pulse {
    50% {
      opacity: 0.5;
    }
  }

  .track {
    flex: 1 1 auto;
    min-width: 34px;
    display: flex;
    align-items: center;
    padding: 3px 5px;
    border-radius: 3px;
    border: var(--border-width) solid var(--glass-border);
    border-left: 2px solid var(--tint);
    background: rgb(var(--bg-1-rgb) / 0.55);
  }
  .tname {
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 11px;
    color: var(--text);
  }

  /* THE CORD. A hairline that starts in the track's own colour and lands in
     the port's accent, so a shared port is a shared colour. Dashed when it
     goes nowhere, amber/red when the far end is not listening, and a bead
     travels it only while notes are actually going out. */
  .cord {
    position: relative;
    flex: 1 1 18px;
    min-width: 14px;
    height: 14px;
    overflow: hidden;
  }
  .cord::before {
    content: "";
    position: absolute;
    left: 0;
    right: 0;
    top: 50%;
    height: 1px;
    background: linear-gradient(to right, var(--tint) 0%, var(--accent) 80%);
    opacity: 0.55;
  }
  .cord.flow::before {
    opacity: 1;
    filter: drop-shadow(0 0 calc(3px * var(--glow-scale)) var(--accent));
  }
  .cord.dead::before {
    background: none;
    border-top: 1px dashed rgb(var(--line-rgb) / 0.3);
    opacity: 1;
  }
  .cord.closed::before {
    background: repeating-linear-gradient(
      to right,
      var(--amber) 0 3px,
      transparent 3px 6px
    );
    opacity: 0.75;
  }
  .cord.missing::before {
    background: repeating-linear-gradient(to right, var(--red) 0 2px, transparent 2px 6px);
    opacity: 0.7;
  }
  .cord.flow::after {
    content: "";
    position: absolute;
    top: 50%;
    margin-top: -2px;
    left: 0;
    width: 8px;
    height: 4px;
    border-radius: 2px;
    background: var(--accent);
    box-shadow: 0 0 calc(5px * var(--glow-scale)) var(--accent);
    animation: cord-flow 900ms linear infinite;
  }
  @keyframes cord-flow {
    from {
      left: -8px;
      opacity: 0;
    }
    25% {
      opacity: 0.9;
    }
    75% {
      opacity: 0.9;
    }
    to {
      left: 100%;
      opacity: 0;
    }
  }
  .cord.thin {
    flex: 1 1 12px;
    min-width: 10px;
    height: 12px;
  }

  .jackbtn {
    flex: 0 1 auto;
    min-width: 0;
    max-width: 62%;
    display: flex;
    padding: 0;
    border: none;
    background: transparent;
    cursor: pointer;
  }
  .jackbtn:hover {
    filter: brightness(1.25);
  }

  .trouble {
    margin: 1px 0 3px 21px;
    font-size: 9px;
    line-height: 1.5;
    color: var(--text-mid);
  }
  .trouble.amber {
    color: var(--amber);
  }
  .trouble.red {
    color: var(--red-soft);
  }
  .trouble.small {
    margin-left: 32px;
    font-size: 8px;
  }
  .inline {
    border: var(--border-width) solid currentColor;
    border-radius: 2px;
    background: transparent;
    color: inherit;
    font-size: 8px;
    letter-spacing: 0.1em;
    padding: 1px 5px;
    margin-left: 3px;
    cursor: pointer;
  }
  .inline:hover {
    background: rgb(var(--amber-rgb) / 0.14);
  }

  /* Clip overrides: subordinate to their track — indented, smaller, hung off
     a branch line. */
  .overs {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin: 2px 0 3px 21px;
  }
  .oheader {
    font-size: 8px;
    letter-spacing: 0.14em;
    color: var(--text-faint);
  }
  .over {
    display: flex;
    align-items: center;
    min-width: 0;
  }
  .branch {
    flex: none;
    width: 9px;
    height: 9px;
    margin-right: 4px;
    border-left: 1px solid rgb(var(--line-rgb) / 0.28);
    border-bottom: 1px solid rgb(var(--line-rgb) / 0.28);
    border-bottom-left-radius: 3px;
    align-self: flex-start;
    margin-top: 2px;
  }
  .clip {
    flex: 0 1 auto;
    min-width: 24px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 10px;
    color: var(--text-mid);
  }

  .intarget .track {
    box-shadow: inset 0 0 0 1px var(--cyan-dim);
  }

  @media (prefers-reduced-motion: reduce) {
    .gatemark.pulse {
      animation: none;
    }
    .cord.flow::after {
      display: none;
    }
  }
</style>
