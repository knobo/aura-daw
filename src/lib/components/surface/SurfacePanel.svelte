<script lang="ts">
  /**
   * Control surface — the bottom-panel faceplate. Mix strips, analog
   * gauges, pad grids that breathe with the meter bus, and homage
   * templates (LPD8). Chrome: emits existing mix/launch commands.
   */
  import { ROLL_RESIZE } from "../../utils/panel-resize";
  import {
    activePage,
    bindable,
    groupWidgets,
    meterTrackId,
    type SurfaceWidget,
  } from "../../utils/control-surface";
  import { formatDb, formatPan } from "../../utils/format";
  import { GAIN_MAX, GAIN_MIN, surface } from "../../state/surface.svelte";
  import { midi } from "../../state/midi.svelte";
  import { project } from "../../state/project.svelte";
  import { plugins } from "../../state/plugins.svelte";
  import { ui } from "../../state/ui.svelte";
  import PanelResizeHandle from "../PanelResizeHandle.svelte";
  import Knob from "../controls/Knob.svelte";
  import Fader from "../controls/Fader.svelte";
  import Gauge from "../controls/Gauge.svelte";
  import Pad from "../controls/Pad.svelte";
  import Lamp from "../controls/Lamp.svelte";
  import BottomPanelTabs from "./BottomPanelTabs.svelte";
  import AddMenu from "./AddMenu.svelte";
  import BindPicker from "./BindPicker.svelte";
  import ChannelStrip from "./ChannelStrip.svelte";
  import ClipList from "./ClipList.svelte";
  import PadGrid from "./PadGrid.svelte";

  const page = $derived(activePage(surface.layout));
  const grouped = $derived(groupWidgets(page));
  const empty = $derived(page.widgets.length === 0);

  $effect(() => {
    surface.hydrate(project.projectDir);
  });

  function trackOf(widget: SurfaceWidget) {
    const t = widget.target;
    if (!t || !("trackId" in t)) return undefined;
    return project.trackById(t.trackId);
  }

  function numericValue(widget: SurfaceWidget): { value: number; min: number; max: number; format?: (v: number) => string; bipolar?: boolean; resetTo?: number } | null {
    const t = widget.target;
    if (!t) return null;
    if (t.kind === "trackGain") {
      const tr = project.trackById(t.trackId);
      if (!tr) return null;
      return { value: tr.gainDb, min: GAIN_MIN, max: GAIN_MAX, format: formatDb, resetTo: 0 };
    }
    if (t.kind === "trackPan") {
      const tr = project.trackById(t.trackId);
      if (!tr) return null;
      return { value: tr.pan, min: -1, max: 1, format: formatPan, bipolar: true, resetTo: 0 };
    }
    if (t.kind === "pluginParam") {
      const p = plugins.paramCache[t.instanceId]?.find((x) => x.id === t.paramId);
      if (!p) return null;
      return { value: p.value, min: p.min, max: p.max, resetTo: p.default };
    }
    return null;
  }

  function writeNumeric(widget: SurfaceWidget, v: number) {
    const t = widget.target;
    if (!t) return;
    if (t.kind === "trackGain") surface.writeGain(t.trackId, v);
    else if (t.kind === "trackPan") surface.writePan(t.trackId, v);
    else if (t.kind === "pluginParam") surface.writePluginParam(t.instanceId, t.paramId, v);
  }

  function pressPad(widget: SurfaceWidget) {
    const t = widget.target;
    if (!t) return;
    // A toggle pad's second press CUTS what it started (`launch_stop`);
    // whether it is lit comes from the overlay, not from a local latch, so
    // a clip that ended on its own leaves the pad ready to fire again.
    if (t.kind === "clipLaunch") {
      if (widget.padMode === "toggle" && surface.isClipPlaying(t.clipId)) {
        void surface.stopClip(t.clipId);
        return;
      }
      void surface.fireClip(t.clipId);
      return;
    }
    if (t.kind === "launchBinding") {
      if (widget.padMode === "toggle" && surface.isBindingPlaying(t.bindingId)) {
        void surface.stopBinding(t.bindingId);
        return;
      }
      void surface.fireBinding(t.bindingId);
      return;
    }
    if (t.kind === "trackMute") {
      const tr = project.trackById(t.trackId);
      if (tr) void surface.writeMute(t.trackId, !tr.muted);
    }
  }

  /** Lit = actually sounding (or actually muted). Never a local guess. */
  function padLit(widget: SurfaceWidget): boolean {
    const t = widget.target;
    if (t?.kind === "clipLaunch") return surface.isClipPlaying(t.clipId);
    if (t?.kind === "launchBinding") return surface.isBindingPlaying(t.bindingId);
    if (t?.kind === "trackMute") return !!project.trackById(t.trackId)?.muted;
    return false;
  }
</script>

<div class="surface glass" style:height="{ui.rollHeight}px">
  <PanelResizeHandle
    axis="y"
    size={ui.rollHeight}
    spec={ROLL_RESIZE}
    label="Resize control surface"
    onresize={(px) => (ui.rollHeight = px)}
  />

  <header class="head">
    <BottomPanelTabs current="surface" />
    <span class="silk title">{page.templateId === "lpd8" ? "LPD8" : page.name}</span>
    <AddMenu />
    <button
      class="mode mono"
      class:on={surface.editMode}
      type="button"
      onclick={() => surface.setEdit(!surface.editMode)}
    >
      {surface.editMode ? "EDIT" : "PLAY"}
    </button>
  </header>

  <div class="deck" class:lpd8={page.templateId === "lpd8"}>
    {#if page.templateId === "lpd8"}
      <div class="lpd8-face grain">
        <div class="brand">
          <span class="wordmark">AURA</span>
          <span class="model silk">LPD8</span>
        </div>
        <div class="lpd8-body">
        <div class="prog">
          {#each ["PROG", "PAD", "CC", "NOTE"] as lamp (lamp)}
            <span class="proglamp silk">{lamp}</span>
          {/each}
        </div>
        <div class="knob-grid">
          {#each page.widgets.filter((w) => w.kind === "knob") as w, i (w.id)}
            {@const num = numericValue(w)}
            <div class="knob-slot">
              {#if surface.editMode}
                <button
                  class="tool"
                  type="button"
                  title="Bind this knob"
                  aria-label="Bind knob {i + 1}"
                  onclick={() => surface.toggleBind(w.id)}>⌖</button
                >
              {/if}
              <Knob
                value={num?.value ?? 0}
                min={num?.min ?? GAIN_MIN}
                max={num?.max ?? GAIN_MAX}
                resetTo={num?.resetTo}
                label={w.label}
                format={num?.format}
                ariaLabel={w.label}
                size={36}
                oninput={(v) => writeNumeric(w, v)}
                onstart={() => surface.openGesture("gain drag")}
                onend={() => surface.closeGesture()}
              />
              {#if surface.editMode && !w.target}
                <span class="silk ghost">K{i + 1}</span>
              {/if}
              {#if surface.editMode && surface.bindFor === w.id}
                <BindPicker widget={w} />
              {/if}
            </div>
          {/each}
        </div>
        {#each page.widgets.filter((w) => w.kind === "padGrid") as w (w.id)}
          <PadGrid widget={w} edit={surface.editMode} />
        {/each}
        </div>
      </div>
    {:else if empty}
      <div class="empty">
        <p class="lead">A mixer and a pad deck. Built from this project in one click.</p>
        <div class="cta">
          <button class="raised" type="button" onclick={() => surface.choose("recipe:all")}>Add all</button>
          <button class="raised" type="button" onclick={() => surface.choose("recipe:tracks")}>Add all tracks</button>
          <button class="raised" type="button" onclick={() => surface.choose("recipe:clips")}>Add all clips</button>
          <button class="raised" type="button" onclick={() => surface.choose("template:lpd8")}>AKAI LPD8</button>
        </div>
      </div>
    {:else}
      <div class="strips">
        {#each grouped.strips as widgets (widgets[0]?.groupId)}
          {@const t = trackOf(widgets.find((w) => w.target && "trackId" in w.target) ?? widgets[0])}
          <ChannelStrip {widgets} track={t} edit={surface.editMode} />
        {/each}
      </div>
      <div class="loose">
        {#each grouped.loose as w (w.id)}
          {#if w.kind === "clipList"}
            <ClipList widgetId={w.id} edit={surface.editMode} />
          {:else if w.kind === "padGrid"}
            <PadGrid widget={w} edit={surface.editMode} />
          {:else}
            <div class="cell">
              {#if surface.editMode}
                {#if bindable(w.kind)}
                  <button
                    class="tool"
                    type="button"
                    title="Bind this {w.kind}"
                    aria-label="Bind {w.label}"
                    onclick={() => surface.toggleBind(w.id)}>⌖</button
                  >
                {/if}
                <button
                  class="kill"
                  type="button"
                  aria-label="Remove {w.label}"
                  onclick={() => surface.remove(w.id)}>×</button
                >
              {/if}
              {#if w.kind === "gauge"}
                {#if w.target && "trackId" in w.target}
                  <Gauge trackId={w.target.trackId} label={w.label} />
                {:else}
                  <span class="silk ghost">{w.label}</span>
                {/if}
              {:else if w.kind === "pad"}
                <Pad
                  label={w.label}
                  meterTrackId={meterTrackId(w, midi.clips)}
                  lit={padLit(w)}
                  disabled={!w.target}
                  ariaLabel={w.label}
                  onpress={() => pressPad(w)}
                />
              {:else if w.kind === "lamp"}
                {@const tr = trackOf(w)}
                <Lamp
                  on={w.lampRole === "mute" ? !!tr?.muted : w.lampRole === "solo" ? !!tr?.soloed : !!tr?.armed}
                  label={w.label}
                  role={w.lampRole ?? "mute"}
                  ariaLabel={w.label}
                  onclick={() => {
                    if (!tr || !w.lampRole) return;
                    if (w.lampRole === "mute") void surface.writeMute(tr.id, !tr.muted);
                    else if (w.lampRole === "solo") void surface.writeSolo(tr.id, !tr.soloed);
                    else void surface.writeArm(tr.id, !tr.armed);
                  }}
                />
              {:else if w.kind === "fader"}
                {@const num = numericValue(w)}
                {#if num}
                  <Fader
                    value={num.value}
                    min={num.min}
                    max={num.max}
                    resetTo={num.resetTo}
                    label={w.label}
                    format={num.format}
                    ariaLabel={w.label}
                    oninput={(v) => writeNumeric(w, v)}
                    onstart={() => surface.openGesture("fader drag")}
                    onend={() => surface.closeGesture()}
                  />
                {:else}
                  <span class="silk ghost">{w.label}</span>
                {/if}
              {:else if w.kind === "knob"}
                {@const num = numericValue(w)}
                {#if num}
                  <Knob
                    value={num.value}
                    min={num.min}
                    max={num.max}
                    resetTo={num.resetTo}
                    bipolar={num.bipolar}
                    label={w.label}
                    format={num.format}
                    ariaLabel={w.label}
                    oninput={(v) => writeNumeric(w, v)}
                    onstart={() => surface.openGesture("knob drag")}
                    onend={() => surface.closeGesture()}
                  />
                {:else}
                  <span class="silk ghost">{w.label}</span>
                {/if}
              {/if}
              {#if surface.editMode && surface.bindFor === w.id}
                <BindPicker widget={w} />
              {/if}
            </div>
          {/if}
        {/each}
      </div>
    {/if}
  </div>
</div>

<style>
  .surface {
    flex: none;
    display: flex;
    flex-direction: column;
    border-left: none;
    border-right: none;
    border-bottom: none;
    background: rgb(var(--bg-sunken-rgb) / 0.92);
    position: relative;
    z-index: 10;
  }
  .head {
    display: flex;
    align-items: center;
    gap: 10px;
    height: 36px;
    padding: 0 12px;
    border-bottom: var(--border-width) solid var(--glass-border);
    flex: none;
  }
  .title {
    color: var(--text-mid);
  }
  .mode {
    margin-left: auto;
    font-size: 9px;
    letter-spacing: 0.16em;
    padding: 3px 8px;
    border-radius: 999px;
    border: var(--border-width) solid transparent;
    background: transparent;
    color: var(--text-dim);
    cursor: pointer;
  }
  .mode.on {
    color: var(--amber);
    border-color: var(--amber);
  }
  .deck {
    flex: 1;
    overflow: auto;
    padding: 12px 14px 16px;
    display: flex;
    flex-wrap: wrap;
    gap: 14px;
    align-items: flex-start;
  }
  .strips,
  .loose {
    display: flex;
    flex-wrap: wrap;
    gap: 12px;
    align-items: flex-start;
  }
  .empty {
    margin: auto;
    text-align: center;
    display: flex;
    flex-direction: column;
    gap: 14px;
    max-width: 420px;
  }
  .lead {
    color: var(--text-mid);
    font-size: 13px;
  }
  .cta {
    display: flex;
    flex-wrap: wrap;
    gap: 8px;
    justify-content: center;
  }
  .cta button {
    padding: 8px 12px;
    cursor: pointer;
    color: var(--text);
  }
  .cell {
    position: relative;
  }
  .kill {
    position: absolute;
    top: -6px;
    right: -6px;
    z-index: 1;
    border: none;
    background: transparent;
    color: var(--text-faint);
    cursor: pointer;
  }
  .tool {
    position: absolute;
    top: -6px;
    left: -6px;
    z-index: 1;
    border: none;
    padding: 0;
    background: transparent;
    color: var(--text-faint);
    font-size: 12px;
    line-height: 1;
    cursor: pointer;
  }
  .tool:hover {
    color: var(--cyan);
  }
  .knob-slot {
    position: relative;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 2px;
  }
  .kill:hover {
    color: var(--red);
  }
  .ghost {
    color: var(--text-faint);
  }

  .lpd8-face {
    display: flex;
    flex-direction: column;
    gap: 14px;
    padding: 16px 20px 14px;
    border-radius: 10px;
    background-color: var(--bg-1);
    background-image: var(--sheen-face);
    box-shadow: var(--bevel-raised), var(--relief-3);
    width: max-content;
    max-width: 100%;
  }
  .brand {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    padding: 0 4px;
  }
  .wordmark {
    font-family: var(--font-ui);
    font-weight: 700;
    letter-spacing: 0.28em;
    font-size: 13px;
    color: var(--text);
  }
  .model {
    letter-spacing: 0.32em;
    color: var(--text-dim);
  }
  /* The device is landscape: a column of mode buttons on the far left, 8
     knobs in 2×4 beside them, the 4×2 pad block on the right. A single row
     of eight knobs above the pads is a different instrument. */
  .lpd8-body {
    display: flex;
    align-items: center;
    gap: 18px;
  }
  .knob-grid {
    display: grid;
    grid-template-columns: repeat(4, auto);
    gap: 10px 12px;
    justify-items: center;
  }
  .prog {
    display: flex;
    flex-direction: column;
    gap: 6px;
    flex: none;
  }
  .proglamp {
    padding: 3px 8px;
    border-radius: 2px;
    background: var(--bg-sunken);
    box-shadow: var(--bevel-inset);
    color: var(--text-faint);
  }
</style>
