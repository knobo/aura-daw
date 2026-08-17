<script lang="ts">
  /**
   * "Which group does this lane belong to?" — the keyboard-and-click path
   * to what drag-and-drop does by hand. Lists the groups already in use, a
   * "new group", and "no group".
   *
   * Picking MOVES the lane next to that group's other members: a group is a
   * contiguous run of lanes (see `buildLaneLayout`), so membership without
   * the move would paint the group in two pieces.
   */
  import { project } from "../state/project.svelte";
  import { arrangementForAssign, groupNames, nextGroupName } from "../utils/lane-arrange";
  import { groupOf } from "../utils/lane-layout";
  import type { TrackState } from "../types/ipc";

  let { track, onclose }: { track: TrackState; onclose: () => void } = $props();

  const current = $derived(groupOf(track));
  const names = $derived(groupNames(project.tracks));

  async function assign(group: string | null) {
    onclose();
    if (group === current) return;
    await project.arrangeLanes(arrangementForAssign(project.tracks, track.id, group));
  }
</script>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<!-- svelte-ignore a11y_click_events_have_key_events -->
<div class="backdrop" onclick={onclose}></div>
<div class="menu glass" role="menu" aria-label="Lane group">
  <div class="head silk">lane group</div>
  <button role="menuitem" onclick={() => assign(null)}>
    <span>no group</span>
    {#if current === null}<span class="mark">✓</span>{/if}
  </button>
  {#each names as name (name)}
    <button role="menuitem" onclick={() => assign(name)}>
      <span>{name}</span>
      {#if current === name}<span class="mark">✓</span>{/if}
    </button>
  {/each}
  <button class="new" role="menuitem" onclick={() => assign(nextGroupName(project.tracks))}
    >+ new group</button
  >
</div>

<style>
  .backdrop {
    position: fixed;
    inset: 0;
    z-index: 89;
  }
  .menu {
    position: absolute;
    top: calc(100% + 4px);
    left: 0;
    z-index: 90;
    min-width: 152px;
    max-height: 260px;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    padding: 5px;
    border-radius: 7px;
    background: rgba(8, 10, 19, 0.96);
  }
  .head {
    padding: 4px 9px 6px;
  }
  .menu button {
    display: flex;
    justify-content: space-between;
    align-items: baseline;
    gap: 14px;
    background: transparent;
    border: none;
    border-radius: 4px;
    padding: 7px 9px;
    font-size: 10px;
    letter-spacing: 0.16em;
    color: var(--text-dim);
    cursor: pointer;
    text-align: left;
  }
  .menu button:hover {
    background: rgba(82, 229, 255, 0.09);
    color: var(--cyan);
  }
  .menu button.new {
    color: var(--text-faint);
    border-top: 1px solid var(--glass-border);
    margin-top: 3px;
    padding-top: 8px;
  }
  .menu button.new:hover {
    color: var(--violet);
  }
  .mark {
    color: var(--cyan);
    letter-spacing: 0;
  }
</style>
