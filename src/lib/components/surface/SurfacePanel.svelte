<script lang="ts">
  /**
   * Control surface — the bottom-panel deck. Mix strips, analog gauges,
   * pad grids that breathe with the meter bus, and homage racks (LPD8 and
   * friends). Chrome: emits existing mix/launch commands.
   */
  import { ROLL_RESIZE } from "../../utils/panel-resize";
  import {
    activePage,
    bindable,
    groupWidgets,
    meterTrackId,
    type SurfaceWidget,
  } from "../../utils/control-surface";
  import { surface } from "../../state/surface.svelte";
  import { midi } from "../../state/midi.svelte";
  import { project } from "../../state/project.svelte";
  import { ui } from "../../state/ui.svelte";
  import { firePlayer, playerById, setTriggerMode, stopPlayer } from "../../state/players.svelte";
  import type { PlayerTriggerMode } from "../../types/ipc";
  import PanelResizeHandle from "../PanelResizeHandle.svelte";
  import Knob from "../controls/Knob.svelte";
  import Fader from "../controls/Fader.svelte";
  import Gauge from "../controls/Gauge.svelte";
  import Pad from "../controls/Pad.svelte";
  import Lamp from "../controls/Lamp.svelte";
  import { prefs } from "../../prefs/prefs.svelte";
  import BottomPanelTabs from "./BottomPanelTabs.svelte";
  import AddMenu from "./AddMenu.svelte";
  import BindPicker from "./BindPicker.svelte";
  import ChannelStrip from "./ChannelStrip.svelte";
  import ClipList from "./ClipList.svelte";
  import PadGrid from "./PadGrid.svelte";
  import Rack from "./Rack.svelte";

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
    if (t.kind === "player") {
      // Gate fires on the pointer pair (see padPointerDown/Up below), never
      // on click — a real <button> also delivers click for the pointerup
      // half of the SAME press, which would double-fire the pad.
      if (triggerMode(t.playerId) === "gate") return;
      void firePlayer(t.playerId);
    }
  }

  function triggerMode(playerId: string): PlayerTriggerMode {
    return playerById(playerId)?.trigger?.mode ?? "oneShot";
  }

  function padPointerDown(widget: SurfaceWidget) {
    const t = widget.target;
    if (t?.kind === "player" && triggerMode(t.playerId) === "gate") void firePlayer(t.playerId);
  }

  function padPointerUp(widget: SurfaceWidget) {
    const t = widget.target;
    if (t?.kind === "player" && triggerMode(t.playerId) === "gate") void stopPlayer(t.playerId);
  }

  const TRIGGER_MODES: readonly PlayerTriggerMode[] = ["oneShot", "gate", "loop"];

  /** V-12's pad inspector, kept to what the pad itself can show: cycling
   * mode is the only control this task adds, on the pad that owns it. */
  function cycleTriggerMode(playerId: string) {
    const cur = triggerMode(playerId);
    const next = TRIGGER_MODES[(TRIGGER_MODES.indexOf(cur) + 1) % TRIGGER_MODES.length];
    void setTriggerMode(playerId, next);
  }

  /** Lit = actually sounding (or actually muted). Never a local guess. */
  function padLit(widget: SurfaceWidget): boolean {
    return surface.isLit(widget.target);
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
    <span class="silk title">{page.name}</span>
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

  <div class="deck">
    {#if empty}
      <div class="empty">
        <p class="lead">A mixer and a pad deck. Built from this project in one click.</p>
        <div class="cta">
          <button class="raised" type="button" onclick={() => surface.choose("recipe:all")}>Add all</button>
          <button class="raised" type="button" onclick={() => surface.choose("recipe:tracks")}>Add all tracks</button>
          <button class="raised" type="button" onclick={() => surface.choose("recipe:clips")}>Add all clips</button>
          <button class="raised" type="button" onclick={() => surface.choose("rack:lpd8")}>AKAI LPD8</button>
        </div>
      </div>
    {:else}
      <div class="racks">
        {#each grouped.racks as rack (rack.groupId)}
          <Rack device={rack.device} groupId={rack.groupId} widgets={rack.widgets} edit={surface.editMode} />
        {/each}
      </div>
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
                {@const pt = w.target?.kind === "player" ? playerById(w.target.playerId) : null}
                {@const padLabel = w.target?.kind === "player" ? (pt?.name ?? w.label) : w.label}
                <Pad
                  label={padLabel}
                  meterTrackId={meterTrackId(w, midi.clips)}
                  lit={padLit(w)}
                  disabled={!w.target}
                  ariaLabel={padLabel}
                  onpress={() => pressPad(w)}
                  onpointerdown={() => padPointerDown(w)}
                  onpointerup={() => padPointerUp(w)}
                />
                {#if w.target?.kind === "player"}
                  {@const playerId = w.target.playerId}
                  <div class="player-tags">
                    <span class="silk source" data-testid="pad-{playerId}-source">{padLabel}</span>
                    {#if pt?.raw}
                      <span class="silk raw" data-testid="pad-{playerId}-raw">RAW</span>
                    {/if}
                    {#if surface.editMode}
                      <button
                        class="silk mode"
                        type="button"
                        title="Trigger mode — click to cycle"
                        aria-label="{padLabel} trigger mode: {triggerMode(playerId)}"
                        onclick={() => cycleTriggerMode(playerId)}
                      >
                        {triggerMode(playerId)}
                      </button>
                    {/if}
                  </div>
                {/if}
              {:else if w.kind === "lamp"}
                {@const tr = trackOf(w)}
                <Lamp
                  on={w.lampRole === "mute" ? !!tr?.muted : w.lampRole === "solo" ? !!tr?.soloed : !!tr?.armed}
                  label={w.label}
                  role={w.lampRole ?? "mute"}
                  variant={prefs.values.stripKeys}
                  ariaLabel={w.label}
                  onclick={() => {
                    if (!tr || !w.lampRole) return;
                    if (w.lampRole === "mute") void surface.writeMute(tr.id, !tr.muted);
                    else if (w.lampRole === "solo") void surface.writeSolo(tr.id, !tr.soloed);
                    else void surface.writeArm(tr.id, !tr.armed);
                  }}
                />
              {:else if w.kind === "fader" || w.kind === "knob"}
                {@const num = surface.numeric(w)}
                {#if !num}
                  <span class="silk ghost">{w.label}</span>
                {:else if w.kind === "fader"}
                  <Fader
                    value={num.value}
                    min={num.min}
                    max={num.max}
                    resetTo={num.resetTo}
                    label={w.label}
                    format={num.format}
                    ariaLabel={w.label}
                    oninput={(v) => surface.write(w, v)}
                    onstart={() => surface.openGesture("fader drag")}
                    onend={() => surface.closeGesture()}
                  />
                {:else}
                  <Knob
                    value={num.value}
                    min={num.min}
                    max={num.max}
                    resetTo={num.resetTo}
                    bipolar={num.bipolar}
                    label={w.label}
                    format={num.format}
                    ariaLabel={w.label}
                    oninput={(v) => surface.write(w, v)}
                    onstart={() => surface.openGesture("knob drag")}
                    onend={() => surface.closeGesture()}
                  />
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
  .racks,
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
  .player-tags {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 2px;
    margin-top: 2px;
  }
  .source {
    color: var(--text-dim);
    max-width: 56px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .raw {
    color: var(--amber);
    letter-spacing: 0.1em;
  }
  .mode {
    border: none;
    background: transparent;
    padding: 0;
    color: var(--text-faint);
    cursor: pointer;
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }
  .mode:hover {
    color: var(--cyan);
  }
  /* Both edit handles were a faint 12px glyph on a transparent ground, and
     the owner could not see the bind one at all — which made binding a pad
     unreachable in practice, not merely hard. They are chips now: a filled
     ground and a border, so the eye reads a control rather than stray
     punctuation. Only shown in edit mode, so the weight costs the playing
     surface nothing. */
  .kill,
  .tool {
    position: absolute;
    top: -6px;
    z-index: 1;
    display: grid;
    place-items: center;
    width: 16px;
    height: 16px;
    padding: 0;
    border: var(--border-width) solid var(--glass-border);
    border-radius: 50%;
    background: rgb(var(--bg-sunken-rgb) / 0.92);
    color: var(--text-mid);
    font-size: 12px;
    line-height: 1;
    cursor: pointer;
  }
  .kill {
    right: -6px;
  }
  .tool {
    left: -6px;
  }
  .tool:hover {
    color: var(--cyan);
    border-color: var(--cyan);
  }
  .kill:hover {
    color: var(--red);
    border-color: var(--red);
  }
  .ghost {
    color: var(--text-faint);
  }

</style>
