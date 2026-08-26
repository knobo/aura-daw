<script lang="ts">
  import type { SurfaceWidget } from "../../utils/control-surface";
  import { meterTrackId, padIndex } from "../../utils/control-surface";
  import { midi } from "../../state/midi.svelte";
  import { project } from "../../state/project.svelte";
  import { surface } from "../../state/surface.svelte";
  import Pad from "../controls/Pad.svelte";

  const { widget, edit = false }: { widget: SurfaceWidget; edit?: boolean } = $props();

  const cols = $derived(widget.cols ?? 4);
  const rows = $derived(widget.rows ?? 2);
  const cells = $derived(widget.cells ?? []);

  function clipOf(index: number) {
    const id = cells[index];
    return id ? midi.clipById(id) : undefined;
  }

  function colorOf(clipId: string | null | undefined): string | undefined {
    if (!clipId) return undefined;
    const clip = midi.clipById(clipId);
    return clip ? project.trackById(clip.trackId)?.color : undefined;
  }
</script>

<section class="grid-wrap grain" style:--cols={cols} style:--rows={rows} aria-label="Pad grid">
  {#if edit}
    <button class="kill" type="button" title="Remove pad grid" onclick={() => surface.remove(widget.id)}>×</button>
  {/if}
  <div class="pads" style:grid-template-columns="repeat({cols}, 56px)">
    {#each Array.from({ length: rows }, (_, r) => r) as r (r)}
      {#each Array.from({ length: cols }, (_, c) => c) as c (c)}
        {@const index = padIndex(cols, rows, c, r)}
        {@const clip = clipOf(index)}
        <Pad
          label={clip?.name ?? `${r * cols + c + 1}`}
          meterTrackId={clip ? meterTrackId({ ...widget, target: { kind: "clipLaunch", clipId: clip.id } }, midi.clips) : null}
          lit={clip ? surface.isClipPlaying(clip.id) : false}
          color={colorOf(clip?.id)}
          disabled={!clip}
          ariaLabel={clip ? `Play ${clip.name}` : "Empty pad"}
          onpress={() => {
            if (clip) void surface.fireClip(clip.id);
          }}
        />
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
    box-shadow: var(--bevel-raised), var(--relief-2);
  }
  .pads {
    display: grid;
    gap: 8px;
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
