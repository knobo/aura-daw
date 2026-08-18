<script lang="ts">
  /**
   * MIDI patchbay — the panel over the hardware.
   *
   * Three sections in the order you need them: IN (the one hardware input,
   * where its notes land), OUT (every port that is open, its clock and its
   * counters), and PATCH — the whole reason this panel exists: every MIDI
   * track as one row of signal flow, so "what is patched where" is a glance,
   * not an investigation. Port colours do that work; the cords carry them.
   *
   * Read-only mirror of the backend's app-config state: nothing here is
   * document state, every change is one `midiIo` setter, and the 500 ms poll
   * that feeds it belongs to MasterBar (always mounted) — a second poller
   * would double every IPC call.
   */
  import { onMount } from "svelte";
  import { backend } from "../../tauri";
  import { midi } from "../../state/midi.svelte";
  import { midiIo } from "../../state/midiio.svelte";
  import { project } from "../../state/project.svelte";
  import type { AudioDevice } from "../../types/ipc";
  import { buildRoutingView, portAccents, PORT_ACCENTS } from "./routing-view";
  import OutPortCard from "./OutPortCard.svelte";
  import PortJack from "./PortJack.svelte";
  import TrackPatchRow from "./TrackPatchRow.svelte";

  /** Audio inputs an external synth can come back on (the return field). */
  let returns = $state<AudioDevice[]>([]);

  const view = $derived(
    buildRoutingView({
      tracks: project.tracks,
      clips: midi.clips,
      outputs: midiIo.outputs,
      outPorts: midiIo.outPorts,
      routes: midiIo.routes,
      targetTrackId: midiIo.targetTrackId,
    }),
  );
  const accents = $derived(portAccents(midiIo.outputs, midiIo.routes));
  const openPorts = $derived(midiIo.outputs.map((o) => o.port));
  const closedPorts = $derived(
    midiIo.outPorts.filter((p) => !midiIo.outputs.some((o) => o.port.id === p.id)),
  );
  const midiTracks = $derived(project.tracks.filter((t) => t.kind === "midi"));
  const outTally = $derived(
    midiIo.outPorts.length === 0
      ? "no ports"
      : `${midiIo.outputs.length} of ${midiIo.outPorts.length} open`,
  );
  const targetName = $derived(
    midiIo.targetTrackId
      ? (project.trackById(midiIo.targetTrackId)?.name ?? midiIo.targetTrackId)
      : "",
  );

  /** The port's accent slot as a CSS colour. Unknown ports stay grey rather
   * than borrowing someone else's identity. */
  function accent(portId: string | null | undefined): string {
    if (!portId) return "var(--text-faint)";
    const slot = accents.get(portId);
    if (slot === undefined) return "var(--text-dim)";
    const name = PORT_ACCENTS[slot % PORT_ACCENTS.length] ?? "";
    if (!name) return "var(--text-dim)";
    return `var(${name.startsWith("--") ? name : `--${name}`})`;
  }

  /* Which ports are actually passing notes right now: notesSent moved between
     two polls. Honest liveness — "open" only means the port exists. The seen
     map is deliberately plain (not $state): reading it must not re-run this. */
  const notesSeen = new Map<string, number>();
  let flowingPorts = $state<Set<string>>(new Set());
  let flowingCount = 0;
  $effect(() => {
    // A closed port's next open (real backend or demo.ts alike) restarts
    // notesSent at 0 — a stale high-water mark here would then read as
    // "not flowing" until traffic climbs back past it. Drop what the
    // current poll no longer reports so a reopen starts from an unknown
    // baseline, same as a port seen for the first time.
    const openIds = new Set(midiIo.outputs.map((o) => o.port.id));
    for (const id of notesSeen.keys()) {
      if (!openIds.has(id)) notesSeen.delete(id);
    }
    const next = new Set<string>();
    for (const o of midiIo.outputs) {
      const prev = notesSeen.get(o.port.id);
      if (prev !== undefined && o.notesSent > prev) next.add(o.port.id);
      notesSeen.set(o.port.id, o.notesSent);
    }
    if (next.size > 0 || flowingCount > 0) {
      flowingPorts = next;
      flowingCount = next.size;
    }
  });
  function flowing(portId: string | null | undefined): boolean {
    return portId ? flowingPorts.has(portId) : false;
  }

  type Tone = "ok" | "warn" | "idle";
  /** What the input is really doing, said plainly. */
  const inState = $derived.by((): { tone: Tone; text: string } => {
    if (midiIo.inPorts.length === 0) {
      return { tone: "idle", text: "No MIDI inputs found. Connect a keyboard and it shows up here." };
    }
    if (!midiIo.inPortId) {
      return { tone: "idle", text: "No port picked — AURA is not listening." };
    }
    if (!midiIo.capturing) {
      return {
        tone: "warn",
        text: "Port picked but nothing is capturing — pick it again, or reconnect the device.",
      };
    }
    if (!midiIo.targetTrackId) {
      return midiIo.monitor
        ? {
            tone: "warn",
            text: "Capturing. You hear the monitor voice — choose a track to play its instrument.",
          }
        : {
            tone: "warn",
            text: "Capturing, but the notes go nowhere. Turn monitor on, or choose a track.",
          };
    }
    if (midiIo.routing) {
      return { tone: "ok", text: `Notes are reaching ${targetName}'s instrument.` };
    }
    return { tone: "ok", text: `Capturing → ${targetName}. Nothing played yet.` };
  });

  onMount(() => {
    // Refresh the device lists: the panel is usually opened right after
    // plugging something in. The poll itself is MasterBar's job.
    void midiIo.loadPorts();
    backend
      .listInputDevices()
      .then((d) => (returns = d))
      .catch(() => {});
  });

  function openPort(id: string) {
    if (id) void midiIo.openOutPort(id);
  }

  function forget(scope: "track" | "clip", id: string) {
    if (scope === "track") void midiIo.setTrackRoute(id, null, 0);
    else void midiIo.setClipRoute(id, null, 0);
  }
</script>

<div class="patchbay">
  <!-- IN — one hardware input, by backend design -->
  <section>
    <header class="mono">
      IN
      <span class="tally silk">{midiIo.capturing ? "capturing" : "idle"}</span>
      <span class="spacer"></span>
      <button
        class="link mono"
        title="Re-read this machine's MIDI ports"
        onclick={() => void midiIo.loadPorts()}
      >
        rescan
      </button>
    </header>

    <div class="row">
      <span
        class="midi-dot"
        class:active={midiIo.active}
        title="Incoming MIDI activity"
        aria-hidden="true"
      ></span>
      <select
        class="grow"
        aria-label="Hardware MIDI input port"
        onchange={(e) => void midiIo.selectInPort(e.currentTarget.value)}
      >
        <option value="" selected={midiIo.inPortId === ""}>None — not listening</option>
        {#each midiIo.inPorts as p (p.id)}
          <option value={p.id} selected={p.id === midiIo.inPortId}>{p.name}</option>
        {/each}
      </select>
    </div>

    <div class="row">
      <label
        class="toggle"
        title="Hear incoming notes through the preview voice, instrument or not"
      >
        <input
          type="checkbox"
          checked={midiIo.monitor}
          disabled={!midiIo.inPortId}
          onchange={(e) => void midiIo.setMonitor(e.currentTarget.checked)}
        />
        <span class="silk">monitor</span>
      </label>
      <span class="arrow silk" aria-hidden="true">→</span>
      <select
        class="grow"
        aria-label="Track that receives incoming notes"
        onchange={(e) => void midiIo.setInputTrack(e.currentTarget.value || null)}
      >
        <option value="" selected={!midiIo.targetTrackId}>No track</option>
        {#each midiTracks as t (t.id)}
          <option value={t.id} selected={t.id === midiIo.targetTrackId}>{t.name}</option>
        {/each}
      </select>
    </div>

    <p class="state" class:ok={inState.tone === "ok"} class:warn={inState.tone === "warn"}>
      {inState.text}
    </p>
  </section>

  <!-- OUT — every port that is open right now -->
  <section>
    <header class="mono">
      OUT
      <span class="tally silk">{outTally}</span>
    </header>

    {#if midiIo.outputs.length === 0 && midiIo.outPorts.length > 0}
      <p class="state">
        No port is open, so nothing AURA plays reaches hardware. Open one below.
      </p>
    {/if}

    <div class="cards">
      {#each midiIo.outputs as o (o.port.id)}
        <OutPortCard status={o} accent={accent(o.port.id)} live={flowing(o.port.id)} />
      {/each}
    </div>

    {#if midiIo.outPorts.length === 0}
      <p class="state">No MIDI outputs found. Connect a device and it shows up here.</p>
    {:else if closedPorts.length > 0}
      <label class="row openrow">
        <span class="silk">open</span>
        <select
          class="grow"
          aria-label="Open another MIDI output port"
          onchange={(e) => {
            openPort(e.currentTarget.value);
            e.currentTarget.value = "";
          }}
        >
          <option value="" selected>a port…</option>
          {#each closedPorts as p (p.id)}
            <option value={p.id}>{p.name}</option>
          {/each}
        </select>
      </label>
    {:else}
      <p class="hint silk">every port on this machine is open</p>
    {/if}
  </section>

  <!-- PATCH — the point of the panel -->
  <section>
    <header class="mono">
      PATCH
      <span class="tally silk">{view.counts.routed}/{view.counts.tracks} patched</span>
      {#if view.counts.unhealthy > 0}
        <span class="tally amber silk">{view.counts.unhealthy} broken</span>
      {/if}
    </header>

    {#if view.rows.length === 0}
      <p class="state">No MIDI tracks yet. Add one and it shows up here to patch.</p>
    {:else}
      <div class="anatomy silk" aria-hidden="true">
        <span>in</span>
        <span class="spacer"></span>
        <span>out</span>
      </div>
      <div class="rows">
        {#each view.rows as row (row.trackId)}
          <TrackPatchRow
            {row}
            {openPorts}
            {closedPorts}
            {returns}
            {accent}
            {flowing}
            capturing={midiIo.capturing}
          />
        {/each}
      </div>
    {/if}
  </section>

  {#if view.orphans.length > 0}
    <footer class="orphans">
      <span class="silk">left over · {view.orphans.length}</span>
      <p class="hint">
        These routes point at a track or clip that is no longer in the project. They send
        nothing; forgetting them is safe.
      </p>
      {#each view.orphans as o (o.scope + o.id)}
        <div class="orphan">
          <span class="oscope silk">{o.scope}</span>
          <span class="oid mono" title={o.id}>{o.id}</span>
          <PortJack target={o.target} accent={accent(o.target.portId)} />
          <button
            class="link mono"
            title="Drop this leftover route"
            onclick={() => forget(o.scope, o.id)}
          >
            forget
          </button>
        </div>
      {/each}
    </footer>
  {/if}
</div>

<style>
  .patchbay {
    display: flex;
    flex-direction: column;
    gap: 14px;
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding-right: 2px;
  }

  section {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }

  header {
    display: flex;
    align-items: baseline;
    gap: 7px;
    font-size: 9px;
    letter-spacing: 0.16em;
    color: var(--cyan);
    border-bottom: var(--border-width) solid var(--glass-border);
    padding-bottom: 3px;
  }
  .tally {
    letter-spacing: 0.12em;
    color: var(--text-dim);
  }
  .tally.amber {
    color: var(--amber);
  }
  .spacer {
    flex: 1;
  }

  .row {
    display: flex;
    align-items: center;
    gap: 6px;
    min-width: 0;
  }
  .grow {
    flex: 1;
    min-width: 0;
  }

  select {
    background: rgb(var(--bg-2-rgb) / 0.8);
    color: var(--text);
    border: var(--border-width) solid var(--glass-border);
    border-radius: 2px;
    font-size: 10px;
    font-family: var(--font-ui);
    padding: 3px 4px;
    min-width: 0;
  }

  /* The MasterBar activity idiom, unchanged: a dim bead that takes the
     primary accent and glows the moment an event lands. */
  .midi-dot {
    flex: none;
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: rgb(var(--line-rgb) / 0.25);
    border: 1px solid var(--glass-border);
    transition: background 80ms linear, box-shadow 80ms linear;
  }
  .midi-dot.active {
    background: var(--cyan);
    border-color: var(--cyan);
    box-shadow: 0 0 calc(6px * var(--glow-scale)) 1px var(--cyan);
    animation: dot-pulse 1s ease-in-out infinite;
  }
  @keyframes dot-pulse {
    50% {
      opacity: 0.5;
    }
  }

  .toggle {
    flex: none;
    display: flex;
    align-items: center;
    gap: 4px;
    cursor: pointer;
  }
  .toggle input {
    accent-color: var(--cyan);
    width: 11px;
    height: 11px;
    margin: 0;
    cursor: pointer;
  }
  .arrow {
    flex: none;
    color: var(--text-dim);
  }

  .state {
    margin: 0;
    font-size: 9px;
    line-height: 1.5;
    color: var(--text-dim);
  }
  .state.ok {
    color: var(--green);
  }
  .state.warn {
    color: var(--amber);
  }

  .cards {
    display: flex;
    flex-direction: column;
    gap: 5px;
  }
  .openrow .silk {
    flex: none;
  }

  .anatomy {
    display: flex;
    align-items: center;
    font-size: 8px;
    letter-spacing: 0.2em;
    color: var(--text-faint);
  }
  .rows {
    display: flex;
    flex-direction: column;
  }

  .orphans {
    display: flex;
    flex-direction: column;
    gap: 4px;
    padding-top: 8px;
    border-top: var(--border-width) solid var(--glass-border);
  }
  .orphan {
    display: flex;
    align-items: center;
    gap: 5px;
    min-width: 0;
  }
  .oscope {
    flex: none;
    font-size: 8px;
    color: var(--text-faint);
  }
  .oid {
    flex: 0 1 auto;
    min-width: 20px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 9px;
    color: var(--text-dim);
  }

  .hint {
    margin: 0;
    font-size: 8px;
    letter-spacing: 0.08em;
    line-height: 1.5;
    color: var(--text-faint);
    text-transform: none;
  }

  .link {
    flex: none;
    border: none;
    background: transparent;
    color: var(--text-dim);
    font-size: 8px;
    letter-spacing: 0.1em;
    text-decoration: underline;
    cursor: pointer;
    padding: 1px 0;
  }
  .link:hover {
    color: var(--cyan);
  }

  @media (prefers-reduced-motion: reduce) {
    .midi-dot.active {
      animation: none;
    }
  }
</style>
