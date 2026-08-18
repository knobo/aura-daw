<script lang="ts">
  /**
   * CLIPS root: the open project's own material — audio clips and MIDI clips
   * — draggable back onto any track. A drop re-imports (audio) or stamps a
   * copy (MIDI) through the landed commands, so it is undoable for free.
   *
   * MIDI clip rows carry their own usage tools: a track delete leaves its
   * MIDI clips behind (orphaned, invisible on any timeline — the backend
   * only prunes audio clips on TrackRemove) rather than removing them, so
   * this is the one place that surfaces and cleans those up.
   */
  import { project } from "../../state/project.svelte";
  import { midi } from "../../state/midi.svelte";
  import { transport } from "../../state/transport.svelte";
  import { view } from "../../state/view.svelte";
  import { toasts } from "../../state/toasts.svelte";
  import { encodeLibraryDrag, groupLiveUsages, nextUsage } from "../../utils/library";
  import type { ClipUsageEntry } from "../../utils/library";
  import { focusAndSelect } from "../../utils/focusAndSelect";
  import LibraryRow from "./LibraryRow.svelte";
  import type { MidiClip } from "../../types/ipc";

  function seconds(samples: number): string {
    return `${(samples / project.sampleRate).toFixed(1)}s`;
  }

  function isUsed(c: MidiClip): boolean {
    return project.trackById(c.trackId) != null;
  }

  /** Every clip name's live placements, grouped once per render — real
   * content-instancing (ADR 0004) isn't wired to any action yet, so a
   * shared name is the closest thing to "the same logical clip" today.
   * A `$derived` map, not a per-row lookup: with N clip rows each doing
   * its own name filter over the whole clip array, a render is O(n²). */
  const usageByName = $derived.by(() =>
    groupLiveUsages(
      midi.clips,
      new Set(project.tracks.map((t) => t.id)),
    ),
  );
  function liveLocations(name: string): ClipUsageEntry[] {
    return usageByName.get(name) ?? [];
  }

  /** Last clip id jumped to, per clip name — lets the Next button resume
   * the cycle from wherever double-click or a previous Next left off. */
  let usageCursor = $state<Record<string, string>>({});

  function jumpTo(loc: ClipUsageEntry) {
    midi.select(loc.id);
    const samples = midi.ticksToSamples(loc.timelineStartTicks);
    const pad = view.width * view.spp * 0.15;
    view.scrollToSamples(Math.max(0, samples - pad));
    void transport.seek(samples);
    usageCursor[loc.name] = loc.id;
  }

  function jumpToFirstUse(c: MidiClip) {
    const locs = liveLocations(c.name);
    if (locs.length === 0) {
      toasts.info("not used anywhere", `"${c.name}" isn't placed on any track`);
      return;
    }
    jumpTo(locs[0]);
  }

  function jumpToNextUse(c: MidiClip) {
    const next = nextUsage(liveLocations(c.name), usageCursor[c.name] ?? null);
    if (next) jumpTo(next);
  }

  let renameId = $state<string | null>(null);
  let renameDraft = $state("");

  function startRename(c: MidiClip) {
    renameId = c.id;
    renameDraft = c.name;
  }
  function commitRename() {
    const id = renameId;
    const name = renameDraft.trim();
    renameId = null;
    if (id && name) void midi.renameClip(id, name);
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
    {@const used = isUsed(c)}
    {@const locs = liveLocations(c.name)}
    <div
      class="row"
      class:on={midi.selectedClipId === c.id}
      role="button"
      tabindex="0"
      title={used
        ? "Double-click: jump to where it's used · F2: rename"
        : "Not placed on any track — orphaned"}
      draggable={true}
      ondragstart={(e) =>
        e.dataTransfer &&
        encodeLibraryDrag(e.dataTransfer, { kind: "projectMidiClip", clipId: c.id })}
      onclick={() => midi.select(c.id)}
      ondblclick={() => jumpToFirstUse(c)}
      onkeydown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          midi.select(c.id);
        } else if (e.key === "F2") {
          e.preventDefault();
          startRename(c);
        }
      }}
    >
      <span class="used" class:on={used} title={used ? "used" : "orphaned"} aria-hidden="true"
        >{used ? "●" : "○"}</span
      >
      <span class="icon" aria-hidden="true">♫</span>
      {#if renameId === c.id}
        <input
          class="name-edit mono"
          value={renameDraft}
          aria-label="Clip name"
          use:focusAndSelect
          onpointerdown={(e) => e.stopPropagation()}
          ondblclick={(e) => e.stopPropagation()}
          oninput={(e) => (renameDraft = e.currentTarget.value)}
          onkeydown={(e) => {
            e.stopPropagation();
            if (e.key === "Enter") commitRename();
            if (e.key === "Escape") renameId = null;
          }}
          onblur={commitRename}
        />
      {:else}
        <span class="label">{c.name}</span>
      {/if}
      <span class="meta silk">{c.notes.length} n</span>
      <div class="acts">
        <button
          type="button"
          class="mini mono"
          title="Rename (F2)"
          onpointerdown={(e) => e.stopPropagation()}
          onclick={(e) => {
            e.stopPropagation();
            startRename(c);
          }}>✎</button
        >
        {#if locs.length > 1}
          <button
            type="button"
            class="mini mono"
            title="Jump to the next place this is used"
            onpointerdown={(e) => e.stopPropagation()}
            onclick={(e) => {
              e.stopPropagation();
              jumpToNextUse(c);
            }}>›</button
          >
        {/if}
        <button
          type="button"
          class="mini danger mono"
          title="Delete this MIDI clip"
          onpointerdown={(e) => e.stopPropagation()}
          onclick={(e) => {
            e.stopPropagation();
            void midi.removeClip(c.id);
          }}>×</button
        >
      </div>
    </div>
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
  .row {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    padding: 5px 6px;
    border: var(--border-width) solid transparent;
    border-radius: 5px;
    background: transparent;
    color: var(--text-dim);
    text-align: left;
    font: inherit;
    cursor: pointer;
  }
  .row:hover {
    background: rgb(var(--edge-rgb) / 0.08);
    color: var(--text);
  }
  .row.on {
    border-color: var(--cyan-dim);
    color: var(--cyan);
    background: rgb(var(--cyan-rgb) / 0.08);
  }
  .used {
    flex: none;
    width: 10px;
    text-align: center;
    color: var(--text-faint);
  }
  .used.on {
    color: var(--cyan);
  }
  .icon {
    flex: none;
    width: 14px;
    text-align: center;
    opacity: 0.8;
  }
  .label {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 12px;
  }
  .name-edit {
    flex: 1;
    min-width: 0;
    background: var(--bg-2);
    border: var(--border-width) solid var(--glass-border);
    color: var(--text);
    font-size: 12px;
    padding: 2px 4px;
  }
  .meta {
    flex: none;
    letter-spacing: 0.08em;
  }
  .acts {
    flex: none;
    display: flex;
    gap: 4px;
  }
  .mini {
    font-size: 10px;
    letter-spacing: 0.06em;
    padding: 2px 5px;
    background: transparent;
    border: var(--border-width) solid var(--glass-border);
    color: var(--text-dim);
    cursor: pointer;
  }
  .mini:hover {
    color: var(--cyan);
    border-color: var(--cyan-dim);
  }
  .mini.danger:hover {
    color: var(--red);
    border-color: var(--red);
  }
</style>
