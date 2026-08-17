<script lang="ts">
  /**
   * Bound-track indicator for a plugin instance. Reads the project tracks
   * (instrumentId = "plugin:<id>"), never the selected track or the
   * instance row's optional trackId cache.
   */
  import { project } from "../../state/project.svelte";
  import { instanceConnectionLabel, tracksBoundToInstance } from "../../utils/plugin-binding";

  let { instanceId }: { instanceId: string } = $props();

  const bound = $derived(tracksBoundToInstance(project.tracks, instanceId));
  const label = $derived(instanceConnectionLabel(project.tracks, instanceId));
</script>

<span
  class="conn mono"
  class:unbound={bound.length === 0}
  title="MIDI track this instance is bound to"
>
  {label}
</span>

<style>
  .conn {
    flex: none;
    font-size: 8px;
    letter-spacing: 0.12em;
    text-transform: uppercase;
    padding: 2px 6px;
    border-radius: 3px;
    border: var(--border-width) solid rgb(var(--green-rgb) / 0.35);
    color: var(--green);
    background: rgb(var(--green-rgb) / 0.08);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    max-width: 100%;
  }
  .conn.unbound {
    color: var(--text-faint);
    border-color: var(--glass-border);
    background: transparent;
  }
</style>
