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

  let {
    track,
    index,
    collapsed = false,
  }: { track: TrackState; index: number; collapsed?: boolean } = $props();
  let pickerOpen = $state(false);
  let groupMenuOpen = $state(false);

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

  function onDepth(b: (typeof autoTargets)[number], e: Event) {
    const v = parseFloat((e.currentTarget as HTMLInputElement).value);
    void modulation.setDepth(b, v);
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
    void project.setGain(track.id, v);
  }
  function onPan(e: Event) {
    const v = parseFloat((e.currentTarget as HTMLInputElement).value);
    void project.setPan(track.id, v);
  }

  /** Gesture boundaries (Plan E Task 14): explicit begin/end brackets a
   * fader/pan drag so the backend coalesces the per-`input`-event commits
   * into one history-bound batch instead of one per pointer move. `oninput`
   * itself (`onGain`/`onPan` above) is unchanged — the coalescing happens
   * backend-side, not by throttling these calls. */
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
    <div class="row top">
      <button
        class="foldbtn mono"
        aria-expanded="true"
        title="Fold lane to a strip"
        aria-label="Fold lane {track.name}"
        onclick={() => lanes.toggleTrack(track.id)}>▾</button
      >
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
        <button
          class="name asbutton"
          title="{track.name} — double-click to rename"
          ondblclick={startRename}>{track.name}</button
        >
      {/if}
      <span class="picker">
        <button
          class="groupchip mono"
          class:on={group !== null}
          aria-haspopup="menu"
          aria-expanded={groupMenuOpen}
          title={group ? `Lane group: ${group}` : "Add this lane to a group"}
          onclick={() => (groupMenuOpen = !groupMenuOpen)}>{group ?? "⌗"}</button
        >
        {#if groupMenuOpen}
          <LaneGroupMenu {track} onclose={() => (groupMenuOpen = false)} />
        {/if}
      </span>
      {#if isAutomation}
        <span class="autotag mono">auto</span>
      {/if}
      {#if track.kind === "midi"}
        <button
          class="instchip mono"
          class:bound={!!instrument}
          class:plugin={!!pluginInst}
          class:stub={pluginInst?.status === "stub"}
          class:crashed={pluginInst?.status === "crashed"}
          title={pluginInst
            ? `Plugin: ${pluginInst.name} (${pluginInst.format}, ${pluginInst.status})${
                patch ? ` — patch: ${patch.bank} / ${patch.name}` : ""
              } — open params`
            : instrument
              ? `Instrument: ${instrument.name} — open browser`
              : "MIDI track — built-in polysynth; assign an instrument"}
          onclick={openInstrumentPanel}
        >
          {#if pluginInst}
            ⚡ {patch?.name ?? pluginInst.name}
          {:else if instrument}
            ⌁ {instrument.name}
          {:else}
            ⌁ polysynth
          {/if}
        </button>
      {/if}
      <button
        class="del"
        title="Remove track"
        aria-label="Remove track {track.name}"
        onclick={() => project.removeTrack(track.id)}>×</button
      >
    </div>

    {#if isAutomation}
      <div class="targets">
        {#each autoTargets as b (b.id)}
          <div class="target">
            <span class="tlabel mono" title={targetLabel(b)}>{targetLabel(b)}</span>
            <input
              class="fader depth"
              type="range"
              min="-1"
              max="1"
              step="0.01"
              value={b.depth}
              style:--fader-pct="{((b.depth + 1) / 2) * 100}%"
              style:--fader-fill="var(--violet)"
              title="Depth"
              aria-label="Depth for {targetLabel(b)}"
              oninput={(e) => onDepth(b, e)}
              onpointerdown={onDepthDown}
              onpointerup={onGestureEnd}
              onpointercancel={onGestureEnd}
            />
            <button
              class="todel"
              title="Remove target"
              aria-label="Remove {targetLabel(b)}"
              onclick={() => modulation.removeBinding(b.id)}>×</button
            >
          </div>
        {/each}
        <span class="picker">
          <button
            class="addtgt mono"
            class:on={pickerOpen}
            aria-haspopup="menu"
            aria-expanded={pickerOpen}
            onclick={() => (pickerOpen = !pickerOpen)}>+ add target</button
          >
          {#if pickerOpen}
            <LanePickerMenu sourceTrackId={track.id} onclose={() => (pickerOpen = false)} />
          {/if}
        </span>
      </div>
    {:else}
    <div class="row mid">
      <div class="toggles">
        {#if track.kind === "midi"}
          <label class="midiout-check mono" title="Publiser som egen MIDI-utgang i Carla/ALSA patchbay">
            <input
              type="checkbox"
              checked={midiIo.isVirtualTrack(track.id)}
              onchange={(e) => void midiIo.setTrackVirtualOutput(track.id, e.currentTarget.checked)}
            />
            OUT
          </label>
        {/if}
        <button
          class="tog arm"
          class:on={track.armed}
          title="Record arm"
          onclick={() => project.toggleArm(track.id)}>R</button
        >
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
        <span class="picker">
          <button
            class="tog auto"
            class:on={modulation.hasVisible(track.id) || pickerOpen}
            title="Show automation lanes"
            aria-haspopup="menu"
            aria-expanded={pickerOpen}
            aria-pressed={modulation.hasVisible(track.id)}
            onclick={() => (pickerOpen = !pickerOpen)}>A</button
          >
          {#if pickerOpen}
            <LanePickerMenu {track} onclose={() => (pickerOpen = false)} />
          {/if}
        </span>
        <AutomationModeSelector
          mode={track.automationMode}
          onchange={(mode) => project.setAutomationMode(track.id, mode)}
        />
      </div>

      <div class="fader-wrap">
        <input
          class="fader"
          type="range"
          min="-60"
          max="12"
          step="0.1"
          value={track.gainDb}
          style:--fader-pct="{gainPct}%"
          style:--fader-fill={track.color}
          title="Gain"
          aria-label="Gain for {track.name}"
          oninput={onGain}
          onpointerdown={onGainPointerDown}
          onpointerup={onGestureEnd}
          onpointercancel={onGestureEnd}
        />
        <span class="db mono">{formatDb(track.gainDb)}</span>
      </div>

      <div class="pan-wrap" title="Pan">
        <span class="silk">pan</span>
        <input
          class="fader pan"
          type="range"
          min="-1"
          max="1"
          step="0.01"
          value={track.pan}
          style:--fader-pct="{((track.pan + 1) / 2) * 100}%"
          style:--fader-fill="var(--violet)"
          aria-label="Pan for {track.name}"
          oninput={onPan}
          onpointerdown={onPanPointerDown}
          onpointerup={onGestureEnd}
          onpointercancel={onGestureEnd}
          ondblclick={() => project.setPan(track.id, 0)}
        />
      </div>
    </div>

    <div class="row meter-row">
      <Meter trackId={track.id} height={10} />
    </div>
    {/if}
  </div>
  {/if}
</div>

<style>
  .header {
    display: flex;
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
  .autotag {
    font-size: 8px;
    letter-spacing: 0.14em;
    color: var(--violet);
    border: var(--border-width) solid rgb(var(--violet-rgb) / 0.4);
    border-radius: 3px;
    padding: 1px 5px;
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

  .row {
    display: flex;
    align-items: center;
    gap: 8px;
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

  .toggles {
    display: flex;
    gap: 3px;
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
  .tog.arm.on {
    color: var(--text-on-accent);
    background: rgb(var(--red-rgb) / 0.8);
    border-color: var(--red);
    box-shadow: 0 0 calc(8px * var(--glow-scale)) rgb(var(--red-rgb) / 0.4);
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
  .midiout-check {
    display: inline-flex;
    align-items: center;
    gap: 2px;
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

  .tog.auto.on {
    color: var(--bg-0);
    background: var(--magenta);
    border-color: var(--magenta);
    box-shadow: 0 0 calc(8px * var(--glow-scale)) rgb(var(--magenta-rgb) / 0.4);
  }

  .fader-wrap {
    flex: 1;
    min-width: 0;
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .fader {
    flex: 1;
    min-width: 0;
  }
  .db {
    font-size: 9px;
    width: 34px;
    text-align: right;
    color: var(--text-dim);
  }

  .pan-wrap {
    display: flex;
    align-items: center;
    gap: 4px;
    width: 64px;
  }
  .pan-wrap .pan {
    width: 44px;
  }

  .meter-row {
    padding-right: 2px;
  }
</style>
