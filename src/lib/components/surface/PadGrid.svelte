<script lang="ts">
  import type { SurfaceTarget, SurfaceWidget } from "../../utils/control-surface";
  import { meterTrackId, padIndex } from "../../utils/control-surface";
  import { midi } from "../../state/midi.svelte";
  import { project } from "../../state/project.svelte";
  import { surface } from "../../state/surface.svelte";
  import { launch } from "../../state/launch.svelte";
  import { firePlayer, playerById, stopPlayer } from "../../state/players.svelte";
  import type { PlayerTriggerMode } from "../../types/ipc";
  import Pad from "../controls/Pad.svelte";
  import BindPicker from "./BindPicker.svelte";

  /** `removable` off inside a rack: the pad block comes off with the rack. */
  const {
    widget,
    edit = false,
    removable = true,
  }: { widget: SurfaceWidget; edit?: boolean; removable?: boolean } = $props();

  const cols = $derived(widget.cols ?? 4);
  const rows = $derived(widget.rows ?? 2);
  const cells = $derived(widget.cells ?? []);

  function cellTarget(index: number): SurfaceTarget | null {
    return cells[index] ?? null;
  }

  function triggerMode(playerId: string): PlayerTriggerMode {
    return playerById(playerId)?.trigger?.mode ?? "oneShot";
  }

  /** What the pad shows, for any target kind a cell can now hold. */
  function cellLabel(index: number, target: SurfaceTarget | null): string {
    if (!target) return `${index + 1}`;
    if (target.kind === "clipLaunch") return midi.clipById(target.clipId)?.name ?? `${index + 1}`;
    if (target.kind === "launchBinding") {
      return launch.bindings.find((b) => b.id === target.bindingId)?.name ?? `${index + 1}`;
    }
    if (target.kind === "player") return playerById(target.playerId)?.name ?? `${index + 1}`;
    return `${index + 1}`;
  }

  function cellColor(target: SurfaceTarget | null): string | undefined {
    if (target?.kind !== "clipLaunch") return undefined;
    const clip = midi.clipById(target.clipId);
    return clip ? project.trackById(clip.trackId)?.color : undefined;
  }

  /** Same dispatch a loose pad's `pressPad` uses (`SurfacePanel.svelte`) — a
   * cell fires whatever it names instead of only a clip. */
  function pressCell(target: SurfaceTarget | null) {
    if (!target) return;
    if (target.kind === "clipLaunch") {
      void surface.fireClip(target.clipId);
      return;
    }
    if (target.kind === "launchBinding") {
      void surface.fireBinding(target.bindingId);
      return;
    }
    if (target.kind === "player") {
      // Gate fires on the pointer pair below, never on click — see
      // SurfacePanel's `pressPad` for why.
      if (triggerMode(target.playerId) === "gate") return;
      void firePlayer(target.playerId);
    }
  }

  function cellPointerDown(target: SurfaceTarget | null) {
    if (target?.kind === "player" && triggerMode(target.playerId) === "gate") void firePlayer(target.playerId);
  }

  function cellPointerUp(target: SurfaceTarget | null) {
    if (target?.kind === "player" && triggerMode(target.playerId) === "gate") void stopPlayer(target.playerId);
  }
</script>

<section class="grid-wrap grain" style:--cols={cols} style:--rows={rows} aria-label="Pad grid">
  {#if edit && removable}
    <button class="kill" type="button" title="Remove pad grid" onclick={() => surface.remove(widget.id)}>×</button>
  {/if}
  <div class="pads" style:grid-template-columns="repeat({cols}, 56px)">
    {#each Array.from({ length: rows }, (_, r) => r) as r (r)}
      {#each Array.from({ length: cols }, (_, c) => c) as c (c)}
        {@const index = padIndex(cols, rows, c, r)}
        {@const target = cellTarget(index)}
        <div class="slot">
          <!-- In edit mode a pad assigns its cell instead of firing it; an
               empty grid from the `+` menu is otherwise eight dead pads. -->
          <Pad
            label={cellLabel(index, target)}
            meterTrackId={target ? meterTrackId({ ...widget, target }, midi.clips) : null}
            lit={surface.isLit(target)}
            color={cellColor(target)}
            disabled={!target && !edit}
            ariaLabel={edit
              ? target
                ? `Assign pad ${index + 1} — now ${cellLabel(index, target)}`
                : `Assign pad ${index + 1}`
              : target
                ? `Play ${cellLabel(index, target)}`
                : "Empty pad"}
            onpress={() => {
              if (edit) surface.toggleBind(surface.cellKey(widget.id, index));
              else pressCell(target);
            }}
            onpointerdown={() => {
              if (!edit) cellPointerDown(target);
            }}
            onpointerup={() => {
              if (!edit) cellPointerUp(target);
            }}
          />
          {#if edit && surface.bindFor === surface.cellKey(widget.id, index)}
            <BindPicker {widget} cell={index} />
          {/if}
        </div>
      {/each}
    {/each}
  </div>
</section>

<style>
  .grid-wrap {
    position: relative;
    padding: 12px;
    border-radius: calc(var(--ctrl-radius) + 4px);
    background-color: var(--bg-1);
    background-image: var(--sheen-face);
    box-shadow: var(--bevel-frame), var(--relief-2);
  }
  .pads {
    display: grid;
    gap: 8px;
  }
  .slot {
    position: relative;
  }
  .kill {
    position: absolute;
    top: 4px;
    right: 6px;
    border: none;
    background: transparent;
    color: var(--text-faint);
    cursor: pointer;
    z-index: 1;
  }
  .kill:hover {
    color: var(--red);
  }
</style>
