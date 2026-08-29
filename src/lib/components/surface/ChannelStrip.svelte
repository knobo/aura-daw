<script lang="ts">
  import type { SurfaceWidget } from "../../utils/control-surface";
  import type { TrackState } from "../../types/ipc";
  import { formatDb, formatPan } from "../../utils/format";
  import { GAIN_MAX, GAIN_MIN, surface } from "../../state/surface.svelte";
  import Fader from "../controls/Fader.svelte";
  import Gauge from "../controls/Gauge.svelte";
  import Knob from "../controls/Knob.svelte";
  import Lamp from "../controls/Lamp.svelte";
  import { prefs } from "../../prefs/prefs.svelte";

  const {
    widgets,
    track,
    edit = false,
  }: {
    widgets: SurfaceWidget[];
    track: TrackState | undefined;
    edit?: boolean;
  } = $props();

  const groupId = $derived(widgets[0]?.groupId ?? "");
  const missing = $derived(!track);
</script>

<section class="strip grain" class:missing aria-label={track ? `Strip ${track.name}` : "Missing track"}>
  {#if edit}
    <button class="kill" type="button" title="Remove strip" onclick={() => surface.removeStrip(groupId)}>×</button>
  {/if}
  <header class="silk name">{track?.name ?? "missing"}</header>
  <div class="lamps" class:bar={prefs.values.stripKeys === "bar"}>
    {#each widgets.filter((w) => w.kind === "lamp") as w (w.id)}
      <Lamp
        on={w.lampRole === "mute" ? !!track?.muted : w.lampRole === "solo" ? !!track?.soloed : !!track?.armed}
        label={w.label}
        role={w.lampRole ?? "mute"}
        variant={prefs.values.stripKeys}
        compact
        ariaLabel="{track?.name ?? "track"} {w.label}"
        onclick={() => {
          if (!track || !w.lampRole) return;
          if (w.lampRole === "mute") void surface.writeMute(track.id, !track.muted);
          else if (w.lampRole === "solo") void surface.writeSolo(track.id, !track.soloed);
          else void surface.writeArm(track.id, !track.armed);
        }}
      />
    {/each}
  </div>
  {#if track}
    {#each widgets.filter((w) => w.kind === "gauge") as w (w.id)}
      <Gauge trackId={track.id} label={w.label} size={72} />
    {/each}
    <div class="pots">
      {#each widgets.filter((w) => w.kind === "fader") as w (w.id)}
        <Fader
          value={track.gainDb}
          min={GAIN_MIN}
          max={GAIN_MAX}
          resetTo={0}
          label={w.label}
          color={track.color}
          format={formatDb}
          ariaLabel="{track.name} level"
          oninput={(v) => surface.writeGain(track.id, v)}
          onstart={() => surface.openGesture("gain drag")}
          onend={() => surface.closeGesture()}
        />
      {/each}
      {#each widgets.filter((w) => w.kind === "knob") as w (w.id)}
        <Knob
          value={track.pan}
          min={-1}
          max={1}
          bipolar
          resetTo={0}
          label={w.label}
          color={track.color}
          format={formatPan}
          ariaLabel="{track.name} pan"
          size={32}
          oninput={(v) => surface.writePan(track.id, v)}
          onstart={() => surface.openGesture("pan drag")}
          onend={() => surface.closeGesture()}
        />
      {/each}
    </div>
  {/if}
</section>

<style>
  .strip {
    position: relative;
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 8px;
    width: 92px;
    padding: 10px 8px 12px;
    border-radius: calc(var(--ctrl-radius) + 2px);
    background-color: var(--bg-1);
    background-image: var(--sheen-face);
    box-shadow: var(--bevel-frame), var(--relief-1);
  }
  .strip.missing {
    opacity: 0.55;
  }
  .name {
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--text-mid);
  }
  /* Three equal columns of whatever is left, rather than three fixed widths
     that have to add up. The row cannot overflow the strip at any label
     length, which the fixed version did by 16px — see Lamp.svelte.

     `margin-inline` rather than a full-bleed `width: 100%`: stopping the
     overflow is not the same as making it look right, and a row that runs
     exactly to the padding edge reads as badly as one that runs past it —
     the strip's other contents (a 72px gauge in a 76px box, a narrower
     fader) all sit inside a margin, and the key row has to sit inside the
     same one or it is the only thing on the strip touching the walls. */
  .lamps {
    display: grid;
    grid-template-columns: repeat(3, 1fr);
    gap: 3px;
    align-self: stretch;
    margin-inline: 4px;
  }
  /* The segmented variant is one switch, so its parts are butted together
     and the slot's rounded ends come from the first and last key. */
  .lamps.bar {
    gap: 0;
  }
  .pots {
    display: flex;
    align-items: flex-end;
    gap: 4px;
  }
  .kill {
    position: absolute;
    top: 2px;
    right: 4px;
    border: none;
    background: transparent;
    color: var(--text-faint);
    cursor: pointer;
    font-size: 14px;
    line-height: 1;
  }
  .kill:hover {
    color: var(--red);
  }
</style>
