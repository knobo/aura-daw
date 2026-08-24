<script lang="ts">
  /**
   * Left-rail track header: color stripe, name, arm/mute/solo, gain fader
   * with dB readout, a pan knob, and a compact per-track VU meter.
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
  import { library } from "../state/library.svelte";
  import { decodeLibraryDrag, hasLibraryDrag } from "../utils/library";
  import { midiIo } from "../state/midiio.svelte";
  import { formatDb, formatPan } from "../utils/format";
  import { groupOf } from "../utils/lane-layout";
  import { focusAndSelect } from "../utils/focusAndSelect";
  import { selectionModeFor } from "../utils/selection-modifiers";
  import { bulkableTracks, fieldValues, nextBulkValue, type BulkField } from "../utils/lane-bulk";
  import Meter from "./Meter.svelte";
  import Knob from "./controls/Knob.svelte";
  import LanePickerMenu from "./LanePickerMenu.svelte";
  import LaneGroupMenu from "./LaneGroupMenu.svelte";
  import AutomationModeSelector from "./AutomationModeSelector.svelte";
  import InsertChain from "./plugins/InsertChain.svelte";
  import SendRack from "./plugins/SendRack.svelte";
  import OutputPicker from "./plugins/OutputPicker.svelte";
  import LanePluginStrip from "./plugins/LanePluginStrip.svelte";

  let {
    track,
    index,
    collapsed = false,
    orderedTrackIds,
  }: {
    track: TrackState;
    index: number;
    collapsed?: boolean;
    /** Visible lane order (Timeline's `layout`, track rows only) — what a
     * shift-click range-extends over. */
    orderedTrackIds: string[];
  } = $props();
  let pickerOpen = $state(false);
  let groupMenuOpen = $state(false);
  let fxPopoverOpen = $state(false);
  /** Plan G2: the sends popover (bus returns this track feeds). */
  let sendPopoverOpen = $state(false);
  /** Plan G2: the output picker (where this track's signal GOES). */
  let outPopoverOpen = $state(false);

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

  /** The bus this track is routed into, if any (Plan G2). */
  const outTarget = $derived(
    track.output ? project.tracks.find((t) => t.id === track.output) : undefined,
  );

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

  // ── selection (4.5) ──
  // Clicking blank header space selects the lane; clicking a control inside
  // it must not ALSO select — a button, a fader, the rename input and the
  // drag grip all have their own meaning for a click, and stacking
  // "and select this lane" on top of every one of them would surprise more
  // than it would help.
  //
  // `selectionModeFor` reads only `shiftKey`/`ctrlKey`/`metaKey`, which
  // `KeyboardEvent` carries too — it is reused as-is for the keyboard path
  // below rather than duplicated or widened.
  function applySelectionGesture(e: { shiftKey: boolean; ctrlKey: boolean; metaKey: boolean }) {
    const mode = selectionModeFor(e);
    if (mode === "add") lanes.extendTo(track.id, orderedTrackIds);
    // Lane selection only defines three gestures (plain/ctrl/shift), unlike
    // clip selection's four — shift+ctrl subtract has no lane analogue yet,
    // so it folds into toggle rather than being silently ignored.
    else if (mode === "toggle" || mode === "subtract") lanes.toggleSelected(track.id);
    else lanes.selectOnly(track.id);
  }

  function onHeaderClick(e: MouseEvent) {
    const el = e.target as HTMLElement;
    if (el.closest("button, input, select, a, [data-lane-grip]")) return;
    applySelectionGesture(e);
  }

  /**
   * Row-level keyboard: Space/Enter select (with the same modifiers as a
   * click), Ctrl/Cmd+A selects every lane, and Up/Down move focus to the
   * next painted row by id (`orderedTrackIds`, not `nextElementSibling` —
   * a folded group sits between two rows in the DOM and must not eat an
   * arrow-press). Guarded to the ROW itself (`e.target === e.currentTarget`):
   * without it, Space on the mute button or Ctrl+A while renaming a lane
   * would bubble here and hijack a keystroke that already means something
   * to the focused child.
   */
  function onHeaderKeydown(e: KeyboardEvent) {
    if (e.target !== e.currentTarget) return;
    if (e.key === " " || e.key === "Enter") {
      e.preventDefault();
      applySelectionGesture(e);
      return;
    }
    if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "a") {
      e.preventDefault();
      lanes.selectAll();
      return;
    }
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      const i = orderedTrackIds.indexOf(track.id);
      const nextId = i < 0 ? undefined : orderedTrackIds[i + (e.key === "ArrowDown" ? 1 : -1)];
      if (!nextId) return;
      e.preventDefault();
      const grid = (e.currentTarget as HTMLElement).closest('[role="grid"]');
      grid?.querySelector<HTMLElement>(`[data-track-row="${nextId}"]`)?.focus();
    }
  }

  /** Roving tabindex: the selected row(s) are reachable by Tab; with
   * nothing selected, the FIRST row is — a grid must always have exactly
   * one entry point, or Tab skips over the lane list entirely. */
  const rowTabIndex = $derived(
    lanes.isSelected(track.id) || (lanes.selection.size === 0 && index === 0) ? 0 : -1,
  );

  /** This lane's own M/S/A applies to the whole selection once the lane is
   * PART of a multi-lane selection — one lane selected behaves exactly like
   * no selection at all. */
  const inBulkSelection = $derived(lanes.isSelected(track.id) && lanes.selection.size > 1);
  const bulkTargets = $derived(
    inBulkSelection ? bulkableTracks(project.tracks, lanes.selection) : [],
  );

  function pressToggle(field: BulkField) {
    if (inBulkSelection) {
      const value = nextBulkValue(fieldValues(bulkTargets, field));
      void project.setTracksState(
        bulkTargets.map((t) => t.id),
        field,
        value,
      );
    } else if (field === "muted") {
      void project.toggleMute(track.id);
    } else if (field === "soloed") {
      void project.toggleSolo(track.id);
    } else {
      void project.toggleArm(track.id);
    }
  }

  function bulkTitle(verb: string): string {
    return inBulkSelection ? `${verb} ${bulkTargets.length} selected lanes` : verb;
  }

  let dropHover = $state(false);

  function onHeaderDragOver(e: DragEvent) {
    if (track.kind === "automation") return;
    if (!hasLibraryDrag(e.dataTransfer)) return;
    e.preventDefault();
    if (e.dataTransfer) e.dataTransfer.dropEffect = "copy";
    dropHover = true;
  }

  function onHeaderDragLeave() {
    dropHover = false;
  }

  function onHeaderDrop(e: DragEvent) {
    dropHover = false;
    if (track.kind === "automation") return;
    const payload = decodeLibraryDrag(e.dataTransfer);
    if (!payload) return;
    e.preventDefault();
    void library.dropOnTrack(payload, track.id, 0);
  }
</script>

<div
  class="header"
  class:picking={pickerOpen || groupMenuOpen}
  class:auto={isAutomation}
  class:collapsed
  class:grouped={group !== null}
  class:dragging={lanes.draggingTrackId === track.id}
  class:selected={lanes.isSelected(track.id)}
  class:drop={dropHover}
  ondragover={onHeaderDragOver}
  ondragleave={onHeaderDragLeave}
  ondrop={onHeaderDrop}
  style:--track-color={track.color}
  role="row"
  aria-selected={lanes.isSelected(track.id)}
  aria-label="Lane {index + 1}: {track.name}"
  aria-rowindex={index + 1}
  data-track-row={track.id}
  data-track-id={track.id}
  tabindex={rowTabIndex}
  onclick={onHeaderClick}
  onkeydown={onHeaderKeydown}
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
    <div class="strip" role="gridcell">
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
          aria-pressed={track.muted}
          title={bulkTitle("Mute")}
          aria-label={bulkTitle("Mute")}
          onclick={() => pressToggle("muted")}>M</button
        >
        <button
          class="tog solo"
          class:on={track.soloed}
          aria-pressed={track.soloed}
          title={bulkTitle("Solo")}
          aria-label={bulkTitle("Solo")}
          onclick={() => pressToggle("soloed")}>S</button
        >
        <LanePluginStrip {track} folded />
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
      <div class="identity-row" role="gridcell" aria-label="Identity for {track.name}">
        <button class="foldbtn mono" aria-expanded="true" title="Fold lane to a strip" aria-label="Fold lane {track.name}" onclick={() => lanes.toggleTrack(track.id)}>▾</button>
        <span class="idx mono">{String(index + 1).padStart(2, "0")}</span>
        {#if renaming}
          <input class="nameedit" use:focusAndSelect bind:value={draft} aria-label="Track name" onkeydown={onNameKeydown} onblur={commitRename} />
        {:else}
          <button class="name asbutton" title="Double-click to rename {track.name}" ondblclick={startRename}>{track.name}</button>
        {/if}
        <button class="del" title="Remove track" aria-label="Remove track {track.name}" onclick={() => project.removeTrack(track.id)}>×</button>
      </div>

      <div class="metadata-row" role="gridcell" aria-label="Routing and FX for {track.name}">
        <span class="picker">
          <!-- Owner note (2026-08-24): the word "Group" was on every lane
               whether or not the lane had one. The VALUE is the news; the
               word is what the tooltip is for. Ungrouped shows a dim
               placeholder so the control is still findable. -->
          <button class="groupchip mono" class:on={group !== null} aria-haspopup="menu" aria-expanded={groupMenuOpen} aria-label={group ? "Change lane group " + group : "Add lane to a group"} title={group ? "Lane group: " + group : "Add this lane to a group"} onclick={() => (groupMenuOpen = !groupMenuOpen)}>{group ?? "GRP"}</button>
          {#if groupMenuOpen}<LaneGroupMenu {track} onclose={() => (groupMenuOpen = false)} />{/if}
        </span>
        {#if isAutomation}
          <span class="kindchip automation-kind mono" title="Automation track — drives bindings, renders no audio">⌁</span>
        {:else if track.kind === "midi"}
          <button class="instchip mono" class:bound={!!instrument} class:plugin={!!pluginInst} class:stub={pluginInst?.status === "stub"} class:crashed={pluginInst?.status === "crashed"} title={pluginInst ? "Open plugin parameters for " + pluginInst.name : instrument ? "Open instrument browser for " + instrument.name : "Assign an instrument"} onclick={openInstrumentPanel}>
            {#if pluginInst}Instrument · {patch?.name ?? pluginInst.name}{:else if instrument}Instrument · {instrument.name}{:else}Instrument · polysynth{/if}
          </button>
        {:else if track.kind === "bus"}
          <span class="kindchip bus-kind mono" title="Return bus — fed by other tracks' sends and outputs">B</span>
        {:else}
          <span class="kindchip mono" title="Audio track">A</span>
        {/if}
        {#if !isAutomation}
          <LanePluginStrip {track} onoverflow={() => (fxPopoverOpen = true)} />
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
          <span class="picker">
            <button
              class="status outchip"
              class:on={outPopoverOpen}
              class:routed={!!track.output}
              title={outTarget
                ? `Output: ${outTarget.name} — this track does not reach the master directly`
                : "Output: Master"}
              aria-haspopup="menu"
              aria-expanded={outPopoverOpen}
              onclick={() => (outPopoverOpen = !outPopoverOpen)}
            >
              →{outTarget ? ` ${outTarget.name}` : " MASTER"}
            </button>
            {#if outPopoverOpen}
              <OutputPicker {track} onclose={() => (outPopoverOpen = false)} />
            {/if}
          </span>
          {#if track.kind !== "bus"}
            <span class="picker">
              <button
                class="status sendchip"
                class:on={sendPopoverOpen}
                title={(track.sends?.length ?? 0) > 0
                  ? `Sends (${track.sends!.length})`
                  : "Send this track into a bus"}
                aria-haspopup="menu"
                aria-expanded={sendPopoverOpen}
                aria-pressed={sendPopoverOpen}
                onclick={() => (sendPopoverOpen = !sendPopoverOpen)}
              >
                SEND{(track.sends?.length ?? 0) > 0 ? ` ${track.sends!.length}` : ""}
              </button>
              {#if sendPopoverOpen}
                <SendRack {track} onclose={() => (sendPopoverOpen = false)} />
              {/if}
            </span>
          {/if}
          <span class="picker metadata-lanes">
            <button class="status lanes" class:on={modulation.hasVisible(track.id) || pickerOpen} title="Show or add automation lanes" aria-haspopup="menu" aria-expanded={pickerOpen} aria-pressed={modulation.hasVisible(track.id)} onclick={() => (pickerOpen = !pickerOpen)}>Lanes</button>
            {#if pickerOpen}<LanePickerMenu {track} onclose={() => (pickerOpen = false)} />{/if}
          </span>
        {/if}
      </div>

      {#if isAutomation}
        <div class="targets" role="gridcell" aria-label="Automation targets for {track.name}">
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
        </div>
      {:else}
        <div class="status-row" role="gridcell" aria-label="Mix status and automation for {track.name}">
          {#if track.kind === "midi"}
            <label class="midiout-check mono" title="Publiser som egen MIDI-utgang i Carla/ALSA patchbay">
              <input type="checkbox" checked={midiIo.isVirtualTrack(track.id)} onchange={(e) => void midiIo.setTrackVirtualOutput(track.id, e.currentTarget.checked)} />
              MIDI OUT
            </label>
          {/if}
          <button class="status arm" class:on={track.armed} aria-pressed={track.armed} title={bulkTitle("Record arm")} aria-label={bulkTitle("Record arm")} onclick={() => pressToggle("armed")}>A</button>
          <button class="status mute" class:on={track.muted} aria-pressed={track.muted} title={bulkTitle("Mute")} aria-label={bulkTitle("Mute")} onclick={() => pressToggle("muted")}>M</button>
          <button class="status solo" class:on={track.soloed} aria-pressed={track.soloed} title={bulkTitle("Solo")} aria-label={bulkTitle("Solo")} onclick={() => pressToggle("soloed")}>S</button>
          <!-- Owner note (2026-08-24): the single-letter controls belong on
               ONE line. The rule is the meaningful separation between two
               groups that answer different questions — what this lane is
               doing right now (A/M/S) versus what its automation lane is
               doing (O/R/W/T/L). -->
          <span class="chip-rule" aria-hidden="true"></span>
          <span class="chip-group">
            <span class="section-label mono" title="Automation mode">Auto</span>
            <AutomationModeSelector mode={track.automationMode} onchange={(mode) => project.setAutomationMode(track.id, mode)} />
          </span>
        </div>

        <div class="level-area" role="gridcell" aria-label="Level controls for {track.name}">
          <div class="level-head"><span class="section-label mono">Level</span><Meter trackId={track.id} height={8} /></div>
          <div class="level-controls">
            <label class="level-control gain-control">
              <span class="control-label">Gain</span>
              <input class="fader" type="range" min="-60" max="12" step="0.1" value={track.gainDb} style:--fader-pct="{gainPct}%" style:--fader-fill={track.color} aria-label="Gain for {track.name}" oninput={onGain} onpointerdown={onGainPointerDown} onpointerup={onGestureEnd} onpointercancel={onGestureEnd} />
              <output class="value mono">{formatDb(track.gainDb)}</output>
            </label>
            <!-- Pan is the one genuinely rotary parameter on a channel
                 strip — bipolar, centre-detented, adjusted in small amounts —
                 so it is the one that gets a knob rather than a slot. -->
            <div class="level-control pan-control">
              <span class="control-label">Pan</span>
              <Knob
                value={track.pan}
                min={-1}
                max={1}
                bipolar
                size={20}
                color="var(--violet)"
                ariaLabel="Pan for {track.name}"
                oninput={(v) => queueGestureWrite(() => project.setPan(track.id, v))}
                onstart={onPanPointerDown}
                onend={onGestureEnd}
              />
              <output class="value pan-value mono">{formatPan(track.pan)}</output>
            </div>
          </div>
        </div>
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
  /* Restrained on purpose: a full accent fill would fight the colour
     stripe for "what is this lane's identity", so selection is a thin
     inset edge plus a faint wash, both off the same token. */
  .header.selected {
    box-shadow: inset 2px 0 0 0 var(--cyan-dim);
    background: linear-gradient(
      to right,
      rgb(var(--cyan-rgb) / 0.1),
      rgb(var(--bg-1-rgb) / 0.15)
    );
  }
  .header.drop {
    outline: 2px solid var(--cyan);
    outline-offset: -2px;
  }
  /* Keyboard focus ring — an OUTLINE, deliberately not another box-shadow,
     so it stays visually distinct from `.selected`'s wash/edge and the two
     compose cleanly when a focused row is also selected. */
  .header:focus-visible {
    outline: 2px solid var(--cyan);
    outline-offset: -2px;
    z-index: 1;
  }
  @media (prefers-reduced-motion: no-preference) {
    .header {
      transition: outline-color 100ms linear, box-shadow 100ms linear;
    }
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
  .status-row {
    display: flex;
    align-items: center;
    min-width: 0;
    flex: none;
    /* A safety valve, not the layout: everything fits one line on a normal
       lane, but a midi track carries an extra MIDI OUT control in this row
       and a narrow lane would otherwise push the automation chips out of
       the header entirely. */
    flex-wrap: wrap;
    row-gap: 2px;
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
    max-width: 96px;
  }
  .metadata-row .instchip {
    flex: 1;
    max-width: none;
  }
  .kindchip {
    /* A one-letter badge now (owner note, 2026-08-24), so it sizes like
       one instead of reserving room for "Automation track". */
    flex: none;
    min-width: 15px;
    text-align: center;
    padding: 2px 4px;
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
  .bus-kind {
    color: var(--cyan);
    border-color: rgb(var(--cyan-rgb) / 0.35);
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
    min-height: 19px;
    gap: 4px;
    padding-left: 20px;
  }
  /* The two groups sit on one line, so the chips size to their content
     instead of each stretching to a third of the row. */
  .status {
    flex: 0 0 auto;
    min-width: 22px;
    height: 19px;
    padding: 0 7px;
    border-radius: 3px;
    border: var(--border-width) solid rgb(var(--edge-rgb) / 0.18);
    background: transparent;
    color: var(--text-faint);
    font-family: var(--font-mono);
    font-size: 8px;
    letter-spacing: 0.04em;
    cursor: pointer;
  }
  .status.arm,
  .status.mute,
  .status.solo {
    flex: none;
    width: 20px;
    min-width: 0;
    padding: 0;
    text-align: center;
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
  .status.outchip {
    min-width: 40px;
    max-width: 96px;
    overflow: hidden;
    white-space: nowrap;
    text-overflow: ellipsis;
  }
  .status.outchip.routed {
    color: var(--amber);
    border-color: rgb(var(--amber-rgb) / 0.45);
  }
  .status.outchip.on {
    color: var(--bg-0);
    background: var(--amber);
    border-color: var(--amber);
  }
  .status.sendchip {
    min-width: 40px;
  }
  .status.sendchip.on {
    color: var(--bg-0);
    background: var(--cyan);
    border-color: var(--cyan);
    box-shadow: 0 0 calc(8px * var(--glow-scale)) rgb(var(--cyan-rgb) / 0.35);
  }
  /* The divider between "what this lane is doing" and "what its
     automation is doing". Cosmetic, so it is aria-hidden. */
  .chip-rule {
    flex: none;
    width: var(--border-width);
    align-self: stretch;
    min-height: 13px;
    margin: 0 3px;
    background: rgb(var(--edge-rgb) / 0.25);
  }
  .chip-group {
    display: flex;
    align-items: center;
    gap: 4px;
    min-width: 0;
  }
  .section-label {
    flex: none;
    color: var(--text-faint);
    font-size: 8px;
    letter-spacing: 0.14em;
    text-transform: uppercase;
  }
  .chip-group .section-label {
    flex: none;
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
    flex: none;
  }
  .level-control .pan-value {
    width: 24px;
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
