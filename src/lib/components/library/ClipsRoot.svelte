<script lang="ts">
  /**
   * CLIPS root: the open project's own material — audio clips and MIDI clips
   * — draggable back onto any track. A drop re-imports (audio) or stamps a
   * copy (MIDI) through the landed commands, so it is undoable for free.
   */
  import { project } from "../../state/project.svelte";
  import { midi } from "../../state/midi.svelte";
  import { encodeLibraryDrag } from "../../utils/library";
  import LibraryRow from "./LibraryRow.svelte";

  function seconds(samples: number): string {
    return `${(samples / project.sampleRate).toFixed(1)}s`;
  }
</script>

<div class="list">
  {#each project.clips as c (c.id)}
    <LibraryRow
      icon="▤"
      label={c.name}
      meta={seconds(c.lengthSamples)}
      draggable
      ondragstart={(e) =>
        e.dataTransfer &&
        encodeLibraryDrag(e.dataTransfer, { kind: "projectAudioClip", clipId: c.id })}
      onclick={() => project.select(c.id)}
    />
  {/each}
  {#each midi.clips as c (c.id)}
    <LibraryRow
      icon="♫"
      label={c.name}
      meta={`${c.notes.length} n`}
      draggable
      ondragstart={(e) =>
        e.dataTransfer &&
        encodeLibraryDrag(e.dataTransfer, { kind: "projectMidiClip", clipId: c.id })}
      onclick={() => midi.select(c.id)}
    />
  {/each}
  {#if project.clips.length === 0 && midi.clips.length === 0}
    <div class="note silk">no clips in this project yet</div>
  {/if}
</div>

<style>
  .list {
    flex: 1;
    min-height: 0;
    overflow-y: auto;
  }
  .note {
    padding: 10px 4px;
    letter-spacing: 0.1em;
  }
</style>
