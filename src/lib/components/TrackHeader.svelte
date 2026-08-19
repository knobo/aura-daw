<script lang="ts">
  /**
   * Left-rail track header: color stripe, name, arm/mute/solo, gain fader
   * with dB readout, pan, and a compact per-track VU meter.
   */
  import type { TrackState } from "../types/ipc";
  import { project } from "../state/project.svelte";
  import { instruments } from "../state/instruments.svelte";
  import { plugins } from "../state/plugins.svelte";
  import { zyn } from "../state/zynpatches.svelte";
  import { openPluginParams } from "../state/plugin-panel";
  import { modulation } from "../state/modulation.svelte";
  import { ui } from "../state/ui.svelte";
  import { lanes } from "../state/lanes.svelte";
  import { midiIo } from "../state/midiio.svelte";
  import { formatDb } from "../utils/format";
  import { groupOf } from "../utils/lane-layout";
  import { focusAndSelect } from "../utils/focusAndSelect";
  import Meter from "./Meter.svelte";
  import LanePickerMenu from "./LanePickerMenu.svelte";
  import LaneGroupMenu from "./LaneGroupMenu.svelte";
  import AutomationModeSelector from "./AutomationModeSelector.svelte";
  import InsertChain from "./plugins/InsertChain.svelte";

  let {
    track,
    index,
    collapsed = false,
  }: { track: TrackState; index: number; collapsed?: boolean } = $props();
  let pickerOpen = $state(false);
  let groupMenuOpen = $state(false);
  let fxPopoverOpen = $state(false);

  // ── rename ──
  // The editor is opened by double-clicking the name, which is the gesture
  // people already try; the store guards the no-op and empty cases, so this
  // component only has to manage focus and the two keys.
  const renaming = $derived(lanes.renamingTrackId === track.id);
  let draft = $state("");

  function startRename() {
    draft = track.name;
    lanes.renamingTrackId = track.id;
  }

  async function commitRename() {
    // Clear FIRST: `blur` fires on Enter too (the input is removed), and a
    // second commit for the same edit would cost a second undo step.
    if (lanes.renamingTrackId !== track.id) return;
    lanes.renamingTrackId = "";
    await project.renameTrack(track.id, draft);
  }

  function onNameKeydown(e: KeyboardEvent) {
    // Stop here rather than bubbling to the window shortcuts: while an
    // editor is open, Escape means "cancel this", not "cancel a clip drag".
    e.stopPropagation();
    if (e.key === "Enter") {
      e.preventDefault();
      void commitRename();
    } else if (e.key === "Escape") {
      e.preventDefault();
      lanes.renamingTrackId = "";
    }
  }

  const group = $derived(groupOf(track));
  const isAutomation = $derived(track.kind === "automation");
  const autoTargets = $derived(isAutomation ? modulation.bindingsFrom(track.id) : []);

  function targetLabel(b: (typeof autoTargets)[number]): string {
    const t = b.target;
    if (t.kind === "trackParam") {
      const name = project.trackById(t.trackId)?.name ?? t.trackId;
      return `→ ${name} · ${t.param}`;
    }
    if (t.kind === "pluginParam") {
      const inst = plugins.instances.find((i) => i.id === t.instanceId);
      return `→ ${inst?.name ?? t.instanceId} · ${t.paramId}`;
    }
    return "→ target";
  }

  let token: Promise<string | undefined> | undefined;
  let gestureTail: Promise<void> = Promise.resolve();

  function settle(operation: Promise<unknown>): Promise<void> {
    return operation.then(
      () => undefined,
      () => undefined,
    );
  }

  function queueGestureWrite(write: () => Promise<unknown>) {
    gestureTail = settle(gestureTail.then(write));
  }

  function onDepth(b: (typeof autoTargets)[number], e: Event) {
    const v = parseFloat((e.currentTarget as HTMLInputElement).value);
    queueGestureWrite(() => modulation.setDepth(b, v));
  }

  function openGesture(label: string) {
    token = gestureTail.then(() => project.beginGesture(label));
    gestureTail = settle(token);
  }
  function closeGesture() {
    const idp = token;
    token = undefined;
    if (!idp) return;
    gestureTail = settle(
      gestureTail.then(async () => {
        const id = await idp;
        await project.endGesture(id);
      }),
    );
  }

  function onDepthDown() {
    openGesture("depth drag");
  }

  const gainPct = $derived(((track.gainDb + 60) / 72) * 100);
  const instrument = $derived(
    track.kind === "midi" ? instruments.byId(track.instrumentId) : undefined,
  );
  /** Plugin-instance binding ("plugin:<id>" refs) — rendered distinctly. */
  const pluginInst = $derived(
    track.kind === "midi" ? plugins.instanceForRef(track.instrumentId) : undefined,
  );
  /** The bank patch loaded into that instance, if any (Zyn). The patch is
   * what a user picks and re-picks, so it — not the constant host name —
   * is what the chip shows once one is loaded. */
  const patch = $derived(pluginInst ? zyn.loaded[pluginInst.id] : undefined);

  function openInstrumentPanel() {
    if (pluginInst) {
      void openPluginParams(pluginInst.id);
    } else {
      ui.dock = "instruments";
    }
  }

  function onGain(e: Event) {
    const v = parseFloat((e.currentTarget as HTMLInputElement).value);
    queueGestureWrite(() => project.setGain(track.id, v));
  }
  function onPan(e: Event) {
    const v = parseFloat((e.currentTarget as HTMLInputElement).value);
    queueGestureWrite(() => project.setPan(track.id, v));
  }

  function formatPan(value: number): string {
    if (Math.abs(value) < 0.005) return "C";
    return String(Math.round(Math.abs(value) * 100)) + (value < 0 ? "L" : "R");
  }

  /** Gesture boundaries (Plan E Task 14): explicit begin/end brackets a
   * fader/pan drag so the backend coalesces the per-`input`-event commits
   * into one history-bound batch instead of one per pointer move. The close
   * waits for every in-flight input write so the backend sees the final value
   * before it commits a Touch gesture. */
  function onGainPointerDown() {
    openGesture("gain drag");
  }
  function onPanPointerDown() {
    openGesture("pan drag");
  }
  function onGestureEnd() {
    closeGesture();
  }
</script>

<div
  class="header"
  class:picking={pickerOpen || groupMenuOpen}
  class:auto={isAutomation}
  class:collapsed
  class:grouped={group !== null}
  class:dragging={lanes.draggingTrackId === track.id}
  style:--track-color={track.color}
>
  <!-- The colour stripe doubles as the drag handle: it is already the
       lane's identity, it runs the full height, and it costs no layout.
       `data-lane-grip` is what Timeline's rail pointer handler looks for —
       the drag lives there because it needs the lane column's geometry. -->
  <div
    class="stripe"
    data-lane-grip={track.id}
    role="presentation"
    title="Drag to reorder — drop on a group's lanes to join it"
  ></div>
  {#if collapsed}
    <!-- Folded: one strip. Everything here has to survive at 22 px, so it
         is the name plus the two toggles people scan for (mute, solo) and
         nothing else. -->
    <div class="strip">
      <span class="idx mono">{String(index + 1).padStart(2, "0")}</span>
      {#if renaming}
        <input
          class="nameedit"
          use:focusAndSelect
          bind:value={draft}
          aria-label="Track name"
          onkeydown={onNameKeydown}
          onblur={commitRename}
        />
      {:else}
        <button class="name asbutton" title="Double-click to rename" ondblclick={startRename}
          >{track.name}</button
        >
      {/if}
      {#if !isAutomation}
        <button
          class="tog mute"
          class:on={track.muted}
          title="Mute"
          onclick={() => project.toggleMute(track.id)}>M</button
        >
        <button
          class="tog solo"
          class:on={track.soloed}
          title="Solo"
          onclick={() => project.toggleSolo(track.id)}>S</button
        >
      {/if}
      <button
        class="foldbtn mono"
        aria-expanded="false"
        title="Unfold lane"
        aria-label="Unfold lane {track.name}"
        onclick={() => lanes.toggleTrack(track.id)}>▸</button
      >
    </div>
  {:else}
    <div class="body">
      <div class="identity-row">
        <button class="foldbtn mono" aria-expanded="true" title="Fold lane to a strip" aria-label="Fold lane {track.name}" onclick={() => lanes.toggleTrack(track.id)}>▾</button>
        <span class="idx mono">{String(index + 1).padStart(2, "0")}</span>
        {#if renaming}
          <input class="nameedit" use:focusAndSelect bind:value={draft} aria-label="Track name" onkeydown={onNameKeydown} onblur={commitRename} />
        {:else}
          <button class="name asbutton" title="Double-click to rename {track.name}" ondblclick={startRename}>{track.name}</button>
        {/if}
        <button class="del" title="Remove track" aria-label="Remove track {track.name}" onclick={() => project.removeTrack(track.id)}>×</button>
      </div>

      <div class="metadata-row">
        <span class="picker">
          <button class="groupchip mono" class:on={group !== null} aria-haspopup="menu" aria-expanded={groupMenuOpen} aria-label={group ? "Change lane group " + group : "Add lane to a group"} title={group ? "Lane group: " + group : "Add this lane to a group"} onclick={() => (groupMenuOpen = !groupMenuOpen)}>{group ? "Group · " + group : "Group · none"}</button>
          {#if groupMenuOpen}<LaneGroupMenu {track} onclose={() => (groupMenuOpen = false)} />{/if}
        </span>
        {#if isAutomation}
          <span class="kindchip automation-kind mono">Automation track</span>
        {:else if track.kind === "midi"}
          <button class="instchip mono" class:bound={!!instrument} class:plugin={!!pluginInst} class:stub={pluginInst?.status === "stub"} class:crashed={pluginInst?.status === "crashed"} title={pluginInst ? "Open plugin parameters for " + pluginInst.name : instrument ? "Open instrument browser for " + instrument.name : "Assign an instrument"} onclick={openInstrumentPanel}>
            {#if pluginInst}Instrument · {patch?.name ?? pluginInst.name}{:else if instrument}Instrument · {instrument.name}{:else}Instrument · polysynth{/if}
          </button>
        {:else}
          <span class="kindchip mono">Audio track</span>
        {/if}
        {#if !isAutomation}
          <span class="picker">
            <button
              class="status fxchip"
              class:on={fxPopoverOpen}
              title={(track.inserts?.length ?? 0) > 0 ? `Effects (${track.inserts!.length})` : "Add effects"}
              aria-haspopup="menu"
              aria-expanded={fxPopoverOpen}
              aria-pressed={fxPopoverOpen}
              onclick={() => (fxPopoverOpen = !fxPopoverOpen)}
            >
              FX{(track.inserts?.length ?? 0) > 0 ? ` ${track.inserts!.length}` : ""}
            </button>
            {#if fxPopoverOpen}
              <InsertChain {track} onclose={() => (fxPopoverOpen = false)} />
            {/if}
          </span>
          <span class="picker metadata-lanes">
            <button class="status lanes" class:on={modulation.hasVisible(track.id) || pickerOpen} title="Show or add automation lanes" aria-haspopup="menu" aria-expanded={pickerOpen} aria-pressed={modulation.hasVisible(track.id)} onclick={() => (pickerOpen = !pickerOpen)}>Lanes</button>
            {#if pickerOpen}<LanePickerMenu {track} onclose={() => (pickerOpen = false)} />{/if}
          </span>
        {/if}
      </div>

      {#if isAutomation}
        <section class="targets" aria-label="Automation targets for {track.name}">
          <div class="section-head">
            <span class="section-label mono">Targets</span>
            <span class="picker">
              <button class="addtgt mono" class:on={pickerOpen} aria-haspopup="menu" aria-expanded={pickerOpen} onclick={() => (pickerOpen = !pickerOpen)}>+ Add target</button>
              {#if pickerOpen}<LanePickerMenu sourceTrackId={track.id} onclose={() => (pickerOpen = false)} />{/if}
            </span>
          </div>
          {#if autoTargets.length === 0}<span class="empty-target">No targets yet. Add one to route this automation.</span>{/if}
          {#each autoTargets as b (b.id)}
            <div class="target">
              <span class="tlabel mono" title={targetLabel(b)}>{targetLabel(b)}</span>
              <label class="depth-control">
                <span class="depth-label mono">Depth {Math.round(b.depth * 100)}%</span>
                <input class="fader depth" type="range" min="-1" max="1" step="0.01" value={b.depth} style:--fader-pct="{((b.depth + 1) / 2) * 100}%" style:--fader-fill="var(--violet)" title="Depth for {targetLabel(b)}" aria-label="Depth for {targetLabel(b)}" oninput={(e) => onDepth(b, e)} onpointerdown={onDepthDown} onpointerup={onGestureEnd} onpointercancel={onGestureEnd} />
              </label>
              <button class="todel" title="Remove target" aria-label="Remove {targetLabel(b)}" onclick={() => modulation.removeBinding(b.id)}>×</button>
            </div>
          {/each}
        </section>
      {:else}
        <div class="status-row" role="group" aria-label="Mix status for {track.name}">
          {#if track.kind === "midi"}
            <label class="midiout-check mono" title="Publiser som egen MIDI-utgang i Carla/ALSA patchbay">
              <input type="checkbox" checked={midiIo.isVirtualTrack(track.id)} onchange={(e) => void midiIo.setTrackVirtualOutput(track.id, e.currentTarget.checked)} />
              MIDI OUT
            </label>
          {/if}
          <button class="status arm" class:on={track.armed} aria-pressed={track.armed} title="Record arm" onclick={() => project.toggleArm(track.id)}>A</button>
          <button class="status mute" class:on={track.muted} aria-pressed={track.muted} title="Mute" onclick={() => project.toggleMute(track.id)}>M</button>
          <button class="status solo" class:on={track.soloed} aria-pressed={track.soloed} title="Solo" onclick={() => project.toggleSolo(track.id)}>S</button>
        </div>

        <div class="automation-row">
          <span class="section-label mono">Automation</span>
          <AutomationModeSelector mode={track.automationMode} onchange={(mode) => project.setAutomationMode(track.id, mode)} />
        </div>

        <section class="level-area" aria-label="Level controls for {track.name}">
          <div class="level-head"><span class="section-label mono">Level</span><Meter trackId={track.id} height={8} /></div>
          <div class="level-controls">
            <label class="level-control gain-control">
              <span class="control-label">Gain</span>
              <input class="fader" type="range" min="-60" max="12" step="0.1" value={track.gainDb} style:--fader-pct="{gainPct}%" style:--fader-fill={track.color} aria-label="Gain for {track.name}" oninput={onGain} onpointerdown={onGainPointerDown} onpointerup={onGestureEnd} onpointercancel={onGestureEnd} />
              <output class="value mono">{formatDb(track.gainDb)}</output>
            </label>
            <label class="level-control pan-control">
              <span class="control-label">Pan</span>
              <input class="fader pan" type="range" min="-1" max="1" step="0.01" value={track.pan} style:--fader-pct="{((track.pan + 1) / 2) * 100}%" style:--fader-fill="var(--violet)" aria-label="Pan for {track.name}" oninput={onPan} onpointerdown={onPanPointerDown} onpointerup={onGestureEnd} onpointercancel={onGestureEnd} ondblclick={() => project.setPan(track.id, 0)} />
              <output class="value pan-value mono">{formatPan(track.pan)}</output>
            </label>
          </div>
        </section>
      {/if}
    </div>
  {/if}
</div>

<style>
  .header {
    display: flex;
    box-sizing: border-box;
    /* flex: none is load-bearing. The rail is a flex column inside the
       scrolling body; without it, a track list taller than the viewport
       gets SHRUNK to fit while the lane column keeps its real height, and
       the two columns silently drift apart row by row. */
    flex: none;
    height: var(--track-height);
    border-bottom: 1px solid rgb(var(--line-rgb) / 0.08);
    background: linear-gradient(to right, rgb(var(--bg-2-rgb) / 0.35), rgb(var(--bg-1-rgb) / 0.15));
  }
  .header.collapsed {
    height: var(--lane-collapsed);
  }
  /* A grouped lane is inset, so membership is visible at a glance without
     spending vertical space on a second border. */
  .header.grouped {
    padding-left: 10px;
    background: linear-gradient(
      to right,
      color-mix(in srgb, var(--track-color) 7%, rgb(var(--bg-2-rgb) / 0.35)),
      rgb(var(--bg-1-rgb) / 0.15)
    );
  }
  .header.picking {
    z-index: 20;
  }
  /* The lane being dragged stays in place but recedes, so the drop
     indicator — not the original row — is what the eye follows. */
  .header.dragging {
    opacity: 0.4;
  }

  /* ── folded lane ── */
  .strip {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 0 8px 0 7px;
  }
  .strip .name {
    font-size: 10px;
  }
  .strip .tog {
    width: 16px;
    height: 14px;
    font-size: 8px;
  }

  .foldbtn {
    width: 13px;
    height: 13px;
    flex: none;
    padding: 0;
    line-height: 1;
    font-size: 8px;
    border: none;
    background: transparent;
    color: var(--text-faint);
    cursor: pointer;
  }
  .foldbtn:hover {
    color: var(--cyan);
  }

  .groupchip {
    flex: none;
    max-width: 74px;
    font-size: 8px;
    letter-spacing: 0.12em;
    padding: 2px 5px;
    border-radius: 3px;
    border: 1px dashed rgb(var(--edge-rgb) / 0.22);
    background: transparent;
    color: var(--text-faint);
    cursor: pointer;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .groupchip.on {
    color: var(--cyan);
    border-style: solid;
    border-color: var(--cyan-dim);
  }
  .groupchip:hover {
    color: var(--cyan);
    border-color: var(--cyan-dim);
  }

  .nameedit {
    flex: 1;
    min-width: 0;
    font-size: 12px;
    font-weight: 500;
    color: var(--text);
    background: rgb(var(--bg-0-rgb) / 0.9);
    border: 1px solid var(--cyan-dim);
    border-radius: 3px;
    padding: 1px 5px;
  }
  /* The name is a <button> so double-click-to-rename is reachable by
     keyboard and announced; it must not look like one. */
  .name.asbutton {
    text-align: left;
    border: none;
    background: transparent;
    padding: 0;
    cursor: text;
    font-family: inherit;
  }
  .targets {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 3px;
  }
  .target {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .tlabel {
    flex: 1;
    min-width: 0;
    font-size: 9px;
    letter-spacing: 0.06em;
    color: var(--text-dim);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .depth {
    width: 72px;
    flex: none;
  }
  .todel {
    width: 14px;
    height: 14px;
    line-height: 1;
    border: none;
    background: transparent;
    color: var(--text-faint);
    font-size: 12px;
    cursor: pointer;
    border-radius: 3px;
    flex: none;
  }
  .todel:hover {
    color: var(--red);
  }
  .addtgt {
    font-size: 8px;
    letter-spacing: 0.12em;
    padding: 2px 6px;
    border-radius: 3px;
    border: var(--border-width) dashed rgb(var(--violet-rgb) / 0.35);
    background: transparent;
    color: var(--text-faint);
    cursor: pointer;
  }
  .addtgt:hover,
  .addtgt.on {
    color: var(--violet);
    border-color: var(--violet);
  }
  /* Still a 3 px mark, but a 9 px drag target: the paint is clipped to the
     content box and the extra width is padding, with a negative margin so
     the body starts exactly where it always did. 3 px is a fine mark and a
     terrible thing to grab. `filter` rather than `box-shadow` for the glow
     — box-shadow would trace the 9 px border box and float free of the
     stripe it belongs to. */
  .stripe {
    width: 9px;
    flex: none;
    padding: 0 3px;
    margin-right: -6px;
    background: var(--track-color);
    background-clip: content-box;
    opacity: 0.85;
    box-shadow: 0 0 calc(8px * var(--glow-scale)) color-mix(in srgb, var(--track-color) 60%, transparent);
    cursor: grab;
    /* The rail scrolls vertically and so does this gesture; without it the
       browser claims the drag as a scroll on touch and precision trackpads. */
    touch-action: none;
  }
  .stripe:active {
    cursor: grabbing;
  }
  .body {
    flex: 1;
    min-width: 0;
    padding: 8px 10px 6px 9px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }


  .idx {
    font-size: 9px;
    color: var(--text-faint);
  }
  .name {
    flex: 1;
    min-width: 0;
    font-size: 12px;
    font-weight: 500;
    letter-spacing: 0.02em;
    color: var(--text);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .instchip {
    max-width: 92px;
    font-size: 8px;
    letter-spacing: 0.08em;
    padding: 2px 6px;
    border-radius: 3px;
    border: var(--border-width) dashed rgb(var(--edge-rgb) / 0.25);
    background: transparent;
    color: var(--text-faint);
    cursor: pointer;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .instchip.bound {
    color: var(--green);
    border-style: solid;
    border-color: rgb(var(--green-rgb) / 0.35);
  }
  .instchip:hover {
    color: var(--cyan);
    border-color: var(--cyan-dim);
  }
  /* plugin-hosted instrument — distinct from sampler (solid violet chip) */
  .instchip.plugin {
    color: var(--violet);
    border-style: solid;
    border-color: rgb(var(--violet-rgb) / 0.45);
    background: rgb(var(--violet-rgb) / 0.09);
    box-shadow: 0 0 calc(6px * var(--glow-scale)) rgb(var(--violet-rgb) / 0.18);
  }
  .instchip.plugin.stub {
    color: var(--amber);
    border-color: rgb(var(--amber-rgb) / 0.4);
    background: rgb(var(--amber-rgb) / 0.07);
    box-shadow: none;
  }
  .instchip.plugin.crashed {
    color: var(--red);
    border-color: rgb(var(--red-rgb) / 0.45);
    background: rgb(var(--red-rgb) / 0.08);
  }
  .instchip.plugin:hover {
    color: var(--cyan);
    border-color: var(--cyan-dim);
  }

  .del {
    width: 16px;
    height: 16px;
    line-height: 1;
    border: none;
    background: transparent;
    color: transparent;
    font-size: 13px;
    cursor: pointer;
    border-radius: 3px;
  }
  .header:hover .del {
    color: var(--text-faint);
  }
  .del:hover {
    color: var(--red);
  }

  .picker {
    position: relative;
  }
  .tog {
    width: 20px;
    height: 18px;
    font-family: var(--font-mono);
    font-size: 9px;
    border-radius: 3px;
    border: var(--border-width) solid rgb(var(--edge-rgb) / 0.15);
    background: transparent;
    color: var(--text-faint);
    cursor: pointer;
  }
  .tog.mute.on {
    color: var(--bg-0);
    background: var(--amber);
    border-color: var(--amber);
  }
  .tog.solo.on {
    color: var(--bg-0);
    background: var(--cyan);
    border-color: var(--cyan);
    box-shadow: 0 0 calc(8px * var(--glow-scale)) rgb(var(--cyan-rgb) / 0.4);
  }

  .fader {
    flex: 1;
    min-width: 0;
  }



  /* Expanded 320 px rail / 132 px lane: each concept gets a stable row. */
  .body {
    padding: 5px 9px 4px 9px;
    gap: 3px;
  }
  .identity-row,
  .metadata-row,
  .status-row,
  .automation-row {
    display: flex;
    align-items: center;
    min-width: 0;
    flex: none;
  }
  .identity-row {
    height: 17px;
    gap: 7px;
  }
  .identity-row .del {
    color: var(--text-faint);
  }
  .metadata-row {
    height: 17px;
    gap: 6px;
    padding-left: 20px;
  }
  .metadata-row .picker {
    flex: none;
  }
  .metadata-row .metadata-lanes {
    margin-left: auto;
  }
  .metadata-row .status {
    min-width: 48px;
    height: 17px;
  }
  .metadata-row .groupchip {
    max-width: 112px;
  }
  .metadata-row .instchip {
    flex: 1;
    max-width: none;
  }
  .kindchip {
    min-width: 0;
    padding: 2px 6px;
    border: var(--border-width) solid rgb(var(--edge-rgb) / 0.18);
    border-radius: 3px;
    color: var(--text-faint);
    font-size: 8px;
    letter-spacing: 0.08em;
  }
  .automation-kind {
    color: var(--violet);
    border-color: rgb(var(--violet-rgb) / 0.35);
  }
  .midiout-check {
    display: inline-flex;
    align-items: center;
    gap: 3px;
    font-size: 9px;
    color: var(--text-dim);
    white-space: nowrap;
  }
  .midiout-check input {
    width: 11px;
    height: 11px;
    margin: 0;
    accent-color: var(--cyan);
  }
  .status-row {
    height: 19px;
    gap: 4px;
    padding-left: 20px;
  }
  .status {
    flex: 1;
    min-width: 48px;
    height: 19px;
    padding: 0 8px;
    border-radius: 3px;
    border: var(--border-width) solid rgb(var(--edge-rgb) / 0.18);
    background: transparent;
    color: var(--text-faint);
    font-family: var(--font-mono);
    font-size: 8px;
    letter-spacing: 0.04em;
    cursor: pointer;
  }
  .status.arm.on {
    color: var(--text-on-accent);
    background: rgb(var(--red-rgb) / 0.8);
    border-color: var(--red);
  }
  .status.mute.on {
    color: var(--bg-0);
    background: var(--amber);
    border-color: var(--amber);
  }
  .status.solo.on {
    color: var(--bg-0);
    background: var(--cyan);
    border-color: var(--cyan);
  }
  .status.lanes.on {
    color: var(--bg-0);
    background: var(--magenta);
    border-color: var(--magenta);
  }
  .status.fxchip {
    min-width: 32px;
  }
  .status.fxchip.on {
    color: var(--bg-0);
    background: var(--violet);
    border-color: var(--violet);
    box-shadow: 0 0 calc(8px * var(--glow-scale)) rgb(var(--violet-rgb) / 0.35);
  }
  .automation-row {
    height: 20px;
    gap: 8px;
    padding-left: 20px;
  }
  .section-label {
    flex: none;
    color: var(--text-faint);
    font-size: 8px;
    letter-spacing: 0.14em;
    text-transform: uppercase;
  }
  .automation-row .section-label {
    width: 62px;
  }
  .level-area {
    flex: 1;
    min-height: 0;
    display: flex;
    flex-direction: column;
    gap: 3px;
    padding: 3px 5px;
    border-top: var(--border-width) solid rgb(var(--edge-rgb) / 0.12);
    background: rgb(var(--bg-0-rgb) / 0.12);
  }
  .level-head {
    display: flex;
    align-items: center;
    gap: 8px;
    min-height: 8px;
  }
  .level-head .section-label {
    width: 34px;
  }
  .level-head :global(.meter) {
    flex: 1;
    min-width: 0;
  }
  .level-controls {
    display: flex;
    align-items: center;
    gap: 9px;
    min-width: 0;
  }
  .level-control {
    display: flex;
    align-items: center;
    gap: 4px;
    min-width: 0;
  }
  .gain-control {
    flex: 1.3;
  }
  .pan-control {
    flex: 1;
  }
  .control-label {
    flex: none;
    color: var(--text-dim);
    font-size: 9px;
  }
  .level-control .fader {
    min-width: 28px;
  }
  .level-control .value {
    width: 34px;
    flex: none;
    color: var(--text-dim);
    font-size: 8px;
    text-align: right;
  }
  .level-control .pan-value {
    width: 24px;
  }
  .targets {
    margin-left: 20px;
    padding: 3px 5px;
    gap: 4px;
    border-top: var(--border-width) solid rgb(var(--edge-rgb) / 0.12);
    background: rgb(var(--bg-0-rgb) / 0.12);
  }
  .section-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 8px;
    flex: none;
  }
  .empty-target {
    color: var(--text-faint);
    font-size: 9px;
    line-height: 1.35;
  }
  .target {
    min-height: 27px;
  }
  .depth-control {
    width: 104px;
    flex: none;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .depth-label {
    color: var(--text-faint);
    font-size: 7px;
    text-align: right;
  }
  .depth-control .depth {
    width: 100%;
  }
</style>
