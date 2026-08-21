<script lang="ts">
  /**
   * Bound-track indicator for a plugin instance. Reads the project tracks
   * (instrumentId = "plugin:<id>"), never the selected track or the
   * instance row's optional trackId cache.
   *
   * Unbound instruments are a bind control, not a dead label — adding a
   * plugin without a track is a real path, and the badge is the chip the
   * rack, params panel and patch browser already show.
   */
  import { project } from "../../state/project.svelte";
  import { plugins } from "../../state/plugins.svelte";
  import { instanceConnectionLabel, tracksBoundToInstance } from "../../utils/plugin-binding";

  let { instanceId }: { instanceId: string } = $props();

  const bound = $derived(tracksBoundToInstance(project.tracks, instanceId));
  const label = $derived(instanceConnectionLabel(project.tracks, instanceId));
  const midiTracks = $derived(project.tracks.filter((t) => t.kind === "midi"));
  const canBind = $derived(bound.length === 0 && midiTracks.length > 0);

  function onBind(e: Event) {
    const sel = e.currentTarget as HTMLSelectElement;
    const trackId = sel.value;
    sel.value = "";
    if (trackId) void plugins.bind(instanceId, trackId);
  }
</script>

{#if canBind}
  <select
    class="conn mono unbound"
    aria-label="Bind to a MIDI track"
    title="Bind this instrument to a MIDI track"
    onchange={onBind}
    onclick={(e) => e.stopPropagation()}
    onpointerdown={(e) => e.stopPropagation()}
  >
    <option value="">bind to track…</option>
    {#each midiTracks as t (t.id)}
      <option value={t.id}>{t.name}</option>
    {/each}
  </select>
{:else}
  <span
    class="conn mono"
    class:unbound={bound.length === 0}
    title="MIDI track this instance is bound to"
  >
    {label}
  </span>
{/if}

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
  select.conn {
    cursor: pointer;
    font: inherit;
    letter-spacing: inherit;
    text-transform: inherit;
    color: inherit;
    max-width: 12rem;
  }
</style>
