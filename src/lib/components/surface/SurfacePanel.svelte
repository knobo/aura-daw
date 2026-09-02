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
    targetKey,
    type SurfaceTarget,
    type SurfaceWidget,
  } from "../../utils/control-surface";
  import { launch } from "../../state/launch.svelte";
  import { surface } from "../../state/surface.svelte";
  import { midi } from "../../state/midi.svelte";
  import { project } from "../../state/project.svelte";
  import { ui } from "../../state/ui.svelte";
  import {
    firePlayer,
    playerById,
    setChokeGroup,
    setQuantize,
    setTriggerMode,
    setVelocityToGain,
    stopPlayer,
  } from "../../state/players.svelte";
  import type { LaunchBinding, PlayerQuantize, PlayerTriggerMode } from "../../types/ipc";
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

  // ── Plan V — V3 ───────────────────────────────────────────────────────
  // Three more cycling chips, the same shape as the trigger-mode one above
  // and for the same reason: a document property nothing can reach is the
  // defect V2's own fix round caught with Gate and Loop. The full inspector
  // — a real control for a continuous depth included — is V7's.

  const QUANTIZE: readonly PlayerQuantize[] = [
    "off",
    "sixteenth",
    "eighth",
    "quarter",
    "whole",
    "bar",
  ];
  /** What each division reads as on a chip a few characters wide. */
  const QUANTIZE_LABEL: Record<PlayerQuantize, string> = {
    off: "Q OFF",
    sixteenth: "Q 1/16",
    eighth: "Q 1/8",
    quarter: "Q 1/4",
    whole: "Q 1/1",
    bar: "Q BAR",
  };
  /** Four groups is what a hand reaches on one deck; `null` is out of every
   * group, and is where the cycle starts and returns. */
  const CHOKE_GROUPS: readonly (number | null)[] = [null, 1, 2, 3, 4];
  /** OFF / half / full, because a pad chip cannot carry a continuous
   * control — the depth itself is a `number` and the command takes any
   * value in 0..=1. */
  const VELOCITY_DEPTHS: readonly number[] = [1, 0.5, 0];

  function quantizeOf(playerId: string): PlayerQuantize {
    return playerById(playerId)?.trigger?.quantize ?? "off";
  }

  // ── quantize on a LAUNCH pad ─────────────────────────────────────────
  // A pad bound to a launch binding fires a SCENE, not a player, and the
  // owner met that half first: the chips only ever showed on a player pad.
  // Quantize is the one of the three that means the same thing on both —
  // a choke or a velocity on a scene would cut or duck real arrangement
  // tracks, which is a different feature and not this one.
  //
  // The lookup has to go through the binding's own target, not the pad's:
  // V2 MIGRATED launch bindings onto players, so a `clipLaunch` pad can
  // resolve to a binding whose target is a `player` — and then the
  // division that governs the press is the player's, because
  // `launch_fire_from` returns early into `player_fire` for exactly that
  // case. Writing the binding's field there would set a value nothing
  // reads.

  /** The binding a pad fires, or `null` — no side effects, so it is safe
   * to call while rendering. A `clipLaunch` pad has no binding until the
   * first press mints one. */
  function bindingOf(t: SurfaceTarget | null | undefined): LaunchBinding | null {
    if (t?.kind === "launchBinding") return launch.bindings.find((b) => b.id === t.bindingId) ?? null;
    if (t?.kind === "clipLaunch") return launch.existingBindingForClip(t.clipId);
    return null;
  }

  /** Does this pad have a quantize division at all? */
  function padTakesQuantize(t: SurfaceTarget | null | undefined): boolean {
    return t?.kind === "player" || t?.kind === "launchBinding" || t?.kind === "clipLaunch";
  }

  /** `null` unless `b` is a migrated binding — a shell over a player (V2)
   * — in which case the player is what actually fires it and its id is
   * the one to write or read quantize through. The one place this check
   * lives; `launch_fire_from` (`control/mod.rs`) is the Rust side of the
   * same rule, at fire time rather than at read/write time. */
  function migratedPlayerId(b: LaunchBinding | null): string | null {
    return b && b.target.kind === "player" ? b.target.playerId : null;
  }

  function padQuantize(t: SurfaceTarget | null | undefined): PlayerQuantize {
    if (t?.kind === "player") return quantizeOf(t.playerId);
    const b = bindingOf(t);
    if (!b) return "off";
    const migrated = migratedPlayerId(b);
    return migrated ? quantizeOf(migrated) : (b.quantize ?? "off");
  }

  /** Cycle it, writing wherever the press actually reads it from.
   *
   * A `clipLaunch` pad with no binding yet MINTS one, the same way its
   * first press would (`surface.fireClip`) — so the chip works on a pad
   * that has never been pressed instead of appearing only after it has.
   * `mapClip` focuses the binding it creates, which is the same jump
   * pressing the pad would have made. */
  async function cyclePadQuantize(t: SurfaceTarget | null | undefined) {
    const next = QUANTIZE[(QUANTIZE.indexOf(padQuantize(t)) + 1) % QUANTIZE.length];
    if (t?.kind === "player") {
      await setQuantize(t.playerId, next);
      return;
    }
    let b = bindingOf(t);
    if (!b && t?.kind === "clipLaunch") b = await launch.mapClip(t.clipId);
    if (!b) return;
    const migrated = migratedPlayerId(b);
    if (migrated) await setQuantize(migrated, next);
    else await launch.setQuantize(b.id, next);
  }

  function chokeOf(playerId: string): number | null {
    return playerById(playerId)?.chokeGroup ?? null;
  }

  function velocityDepthOf(playerId: string): number {
    return playerById(playerId)?.velocityToGain ?? 1;
  }

  function cycleChokeGroup(playerId: string) {
    const at = CHOKE_GROUPS.indexOf(chokeOf(playerId));
    void setChokeGroup(playerId, CHOKE_GROUPS[(at + 1) % CHOKE_GROUPS.length]);
  }

  /** Nearest step, not `indexOf`: the depth is a number the command accepts
   * anywhere in 0..=1, so a document written by anything but this chip
   * would otherwise fall off the cycle and always jump to the first step. */
  function cycleVelocityDepth(playerId: string) {
    const cur = velocityDepthOf(playerId);
    let at = 0;
    for (let i = 1; i < VELOCITY_DEPTHS.length; i += 1) {
      if (Math.abs(VELOCITY_DEPTHS[i] - cur) < Math.abs(VELOCITY_DEPTHS[at] - cur)) at = i;
    }
    void setVelocityToGain(playerId, VELOCITY_DEPTHS[(at + 1) % VELOCITY_DEPTHS.length]);
  }

  function velocityLabel(playerId: string): string {
    const d = velocityDepthOf(playerId);
    return d === 0 ? "VEL OFF" : `VEL ${Math.round(d * 100)}%`;
  }

  /** Lit = actually sounding (or actually muted). Never a local guess. */
  function padLit(widget: SurfaceWidget): boolean {
    return surface.isLit(widget.target);
  }
</script>

{#snippet quantizeChip(target: SurfaceTarget, label: string, keySuffix: string)}
  {@const q = padQuantize(target)}
  <button
    class="silk mode"
    type="button"
    data-testid="pad-{keySuffix}-quantize"
    title="Quantize the press to the arrangement's grid — click to cycle"
    aria-label="{label} quantize: {q}"
    onclick={() => cyclePadQuantize(target)}
  >
    {QUANTIZE_LABEL[q]}
  </button>
{/snippet}

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
                <!-- A pad naming a player nothing has any more must LOOK
                     unbound: left enabled it is indistinguishable from a
                     working pad and does nothing when pressed, which is the
                     silent failure this branch exists to stop. -->
                {@const dead = w.target?.kind === "player" && !pt}
                <Pad
                  label={padLabel}
                  meterTrackId={meterTrackId(w, midi.clips)}
                  lit={padLit(w)}
                  disabled={!w.target || dead}
                  ariaLabel={padLabel}
                  onpress={() => pressPad(w)}
                  onpointerdown={() => padPointerDown(w)}
                  onpointerup={() => padPointerUp(w)}
                />
                {#if surface.editMode && w.target && w.target.kind !== "player" && padTakesQuantize(w.target)}
                  <div class="player-tags">
                    {@render quantizeChip(w.target, padLabel, targetKey(w.target) ?? w.id)}
                  </div>
                {/if}
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
                      {@render quantizeChip(w.target, padLabel, playerId)}
                      <button
                        class="silk mode"
                        type="button"
                        data-testid="pad-{playerId}-choke"
                        title="Choke group — pads in one group cut each other"
                        aria-label="{padLabel} choke group: {chokeOf(playerId) ?? 'none'}"
                        onclick={() => cycleChokeGroup(playerId)}
                      >
                        {chokeOf(playerId) === null ? "CHK —" : `CHK ${chokeOf(playerId)}`}
                      </button>
                      <button
                        class="silk mode"
                        type="button"
                        data-testid="pad-{playerId}-velocity"
                        title="How much of a press's velocity reaches the output"
                        aria-label="{padLabel} velocity depth: {velocityLabel(playerId)}"
                        onclick={() => cycleVelocityDepth(playerId)}
                      >
                        {velocityLabel(playerId)}
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
