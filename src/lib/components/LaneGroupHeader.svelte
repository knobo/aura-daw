<script lang="ts">
  /**
   * The rail half of a lane group: a thin strip carrying the fold triangle,
   * the group's name (double-click to rename) and a member count.
   *
   * The group IS its name (see `TrackState.group`), so renaming here
   * rewrites every member's `group` in one transaction — which is also why
   * the editor commits through `project.arrangeLanes` rather than a
   * per-track call.
   */
  import { project } from "../state/project.svelte";
  import { lanes } from "../state/lanes.svelte";
  import { toasts } from "../state/toasts.svelte";
  import { arrangementForGroupDissolve, arrangementForGroupRename } from "../utils/lane-arrange";
  import { focusAndSelect } from "../utils/focusAndSelect";
  import type { LaneRow } from "../utils/lane-layout";

  let { row }: { row: Extract<LaneRow, { kind: "group" }> } = $props();

  const editing = $derived(lanes.renamingGroup === row.group);
  let draft = $state("");

  function startRename() {
    draft = row.group;
    lanes.renamingGroup = row.group;
  }

  async function commit() {
    // Clear FIRST: `blur` fires on Enter too (the input is removed), and a
    // second commit for the same edit would cost a second undo step. The
    // guard makes that second, blur-triggered call a no-op — which is also
    // what makes Escape's clear (below) actually cancel instead of
    // committing the draft anyway.
    if (lanes.renamingGroup !== row.group) return;
    const from = row.group;
    const to = draft.trim();
    lanes.renamingGroup = "";
    if (!to || to === from) return;
    const arrangement = arrangementForGroupRename(project.tracks, from, to);
    if (!arrangement) {
      toasts.error("RENAME FAILED", `A group named "${to}" already exists`);
      return;
    }
    // Carry the fold state to the new name BEFORE the store changes, so the
    // group does not visibly pop open for a frame on rename. Rolled back
    // below if the arrangement doesn't actually go through.
    lanes.renameGroupFold(from, to);
    const ok = await project.arrangeLanes(arrangement);
    if (!ok) lanes.renameGroupFold(to, from);
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Enter") {
      e.preventDefault();
      void commit();
    } else if (e.key === "Escape") {
      e.preventDefault();
      lanes.renamingGroup = "";
    }
  }

  async function dissolve() {
    await project.arrangeLanes(arrangementForGroupDissolve(project.tracks, row.group));
  }
</script>

<div class="grouphead" class:folded={row.collapsed} style:--group-color={row.color}>
  <button
    class="fold mono"
    aria-expanded={!row.collapsed}
    title={row.collapsed ? "Unfold group" : "Fold group"}
    aria-label="{row.collapsed ? 'Unfold' : 'Fold'} group {row.group}"
    onclick={() => lanes.toggleGroup(row.group)}>{row.collapsed ? "▶" : "▼"}</button
  >
  {#if editing}
    <input
      class="nameedit mono"
      use:focusAndSelect
      bind:value={draft}
      aria-label="Group name"
      onkeydown={onKeydown}
      onblur={commit}
    />
  {:else}
    <button class="name mono" title="Double-click to rename" ondblclick={startRename}
      >{row.group}</button
    >
  {/if}
  <span class="count mono">{row.trackIds.length}</span>
  <button class="dissolve" title="Ungroup these lanes" aria-label="Ungroup {row.group}" onclick={dissolve}
    >⊘</button
  >
</div>

<style>
  .grouphead {
    display: flex;
    align-items: center;
    gap: 6px;
    height: var(--lane-group-height);
    flex: none;
    padding: 0 8px 0 4px;
    border-bottom: 1px solid rgb(var(--line-rgb) / 0.14);
    /* The group's tint runs left-to-right so the strip reads as a header
       for what is below it, not as another lane. */
    background: linear-gradient(
      to right,
      color-mix(in srgb, var(--group-color) 22%, transparent),
      rgb(var(--bg-1-rgb) / 0.35) 65%
    );
  }
  .fold {
    width: 14px;
    height: 14px;
    flex: none;
    line-height: 1;
    font-size: 8px;
    border: none;
    background: transparent;
    color: var(--text-dim);
    cursor: pointer;
    padding: 0;
  }
  .fold:hover {
    color: var(--cyan);
  }
  .name {
    flex: 1;
    min-width: 0;
    text-align: left;
    border: none;
    background: transparent;
    padding: 0;
    font-size: 9px;
    font-weight: 600;
    letter-spacing: 0.16em;
    text-transform: uppercase;
    color: var(--text);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    cursor: text;
  }
  .nameedit {
    flex: 1;
    min-width: 0;
    font-size: 9px;
    letter-spacing: 0.16em;
    text-transform: uppercase;
    color: var(--text);
    background: rgb(var(--bg-0-rgb) / 0.85);
    border: 1px solid var(--cyan-dim);
    border-radius: 3px;
    padding: 1px 4px;
  }
  .count {
    font-size: 8px;
    color: var(--text-faint);
    flex: none;
  }
  .dissolve {
    width: 14px;
    height: 14px;
    flex: none;
    line-height: 1;
    font-size: 10px;
    border: none;
    background: transparent;
    color: transparent;
    cursor: pointer;
    padding: 0;
  }
  .grouphead:hover .dissolve {
    color: var(--text-faint);
  }
  .dissolve:hover {
    color: var(--red);
  }
</style>
