<script lang="ts">
  /**
   * Floating launch-map window: every MIDI note binding and every marked
   * region, click-to-jump, create / update / delete while the panel is open.
   */
  import { launch } from "../../state/launch.svelte";
  import { midi } from "../../state/midi.svelte";
  import { project } from "../../state/project.svelte";
  import { MIDI_NOTES, midiNoteName } from "../../utils/launch-map";
  import type { LaunchBinding } from "../../types/ipc";

  let pos = $state({ x: 24, y: 72 });
  let drag: { dx: number; dy: number } | null = null;

  function onTitleDown(e: PointerEvent) {
    if (e.button !== 0) return;
    if ((e.target as HTMLElement).closest("button")) return;
    drag = { dx: e.clientX - pos.x, dy: e.clientY - pos.y };
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
  }
  function onTitleMove(e: PointerEvent) {
    if (!drag) return;
    pos = {
      x: Math.max(8, e.clientX - drag.dx),
      y: Math.max(8, e.clientY - drag.dy),
    };
  }
  function onTitleUp() {
    drag = null;
  }

  function targetLabel(b: LaunchBinding): string {
    if (b.target.kind === "clip") {
      const clip = midi.clipById(b.target.clipId);
      return clip ? `clip · ${clip.name}` : "clip · missing";
    }
    const names = b.target.trackIds
      .map((id) => project.trackById(id)?.name ?? "?")
      .join(", ");
    const bars = (b.target.lengthTicks / midi.ppq / project.timeSignature[0]).toFixed(1);
    return `${names} · ${bars} bar`;
  }

  function onRowClick(id: string) {
    launch.selectedId = id;
  }

  function onRowDblClick(id: string) {
    void launch.preview(id);
  }

  function playScene(id: string) {
    launch.selectedId = id;
    void launch.preview(id);
  }

  function startNameEdit(id: string, name: string) {
    editNameId = id;
    editNameDraft = name;
  }

  function commitNameEdit() {
    const id = editNameId;
    const name = editNameDraft.trim();
    editNameId = null;
    if (id && name) void launch.update(id, { name });
  }

  function onNoteChange(id: string, raw: string) {
    const note = Number(raw);
    if (!Number.isInteger(note) || note < 0 || note > 127) return;
    void launch.update(id, { note });
  }

  function onChannelChange(id: string, raw: string) {
    if (raw === "") {
      void launch.update(id, { channel: null });
      return;
    }
    const ch = Number(raw);
    if (!Number.isInteger(ch) || ch < 0 || ch > 15) return;
    void launch.update(id, { channel: ch });
  }

  let renameId = $state<string | null>(null);
  let renameDraft = $state("");
  let editNameId = $state<string | null>(null);
  let editNameDraft = $state("");

  async function mapSelectedClip() {
    const id = midi.selectedClipId;
    if (!id) return;
    const b = await launch.mapClip(id);
    if (b) launch.focus(b.id);
  }

  async function useSelectedClip() {
    const id = midi.selectedClipId;
    if (!id) return;
    await launch.setClipLauncher(id, launch.activeMap.id);
  }

  function startRename(id: string, name: string) {
    renameId = id;
    renameDraft = name;
  }

  function commitRename() {
    const id = renameId;
    const name = renameDraft;
    renameId = null;
    if (id) void launch.renameMap(id, name);
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.stopPropagation();
      if (editNameId) editNameId = null;
      else if (launch.learningId) launch.stopLearn();
      else if (launch.marking) launch.marking = false;
      else if (launch.selectedId) launch.selectedId = null;
      else launch.closePanel();
      return;
    }
    if (e.key === "F2" && launch.selectedId) {
      const b = launch.bindings.find((x) => x.id === launch.selectedId);
      if (b) {
        e.preventDefault();
        startNameEdit(b.id, b.name);
      }
      return;
    }
    if (e.key === "Delete" || e.key === "Backspace") {
      // Own the key so a leftover window handler cannot steal Backspace
      // from a rename field, the piano roll, or a clip delete.
      e.stopPropagation();
      const tag = (e.target as HTMLElement)?.tagName;
      if (tag === "INPUT" || tag === "SELECT" || tag === "TEXTAREA") return;
      if (!launch.selectedId) return;
      e.preventDefault();
      void launch.remove(launch.selectedId);
    }
  }
</script>

{#if launch.panelOpen}
  <div
    class="win glass"
    role="dialog"
    aria-label="MIDI launch map"
    style:left="{pos.x}px"
    style:top="{pos.y}px"
    tabindex="-1"
    onkeydown={onKeydown}
  >
    <div
      class="title"
      onpointerdown={onTitleDown}
      onpointermove={onTitleMove}
      onpointerup={onTitleUp}
    >
      <span class="titlelab mono">MIDI LAUNCH</span>
      <span class="sub silk">{launch.activeMap.name} · {launch.bindings.length} marking{launch.bindings.length === 1 ? "" : "s"}</span>
      <button
        class="x mono"
        type="button"
        title="Close"
        aria-label="Close MIDI launch"
        onpointerdown={(e) => {
          e.stopPropagation();
          e.preventDefault();
          launch.closePanel();
        }}>×</button
      >
    </div>

    <div class="maps" role="tablist" aria-label="Launchers">
      {#each launch.maps as m (m.id)}
        {#if renameId === m.id}
          <input
            class="maptab mapname mono"
            class:on={true}
            value={renameDraft}
            aria-label="Launcher name"
            onpointerdown={(e) => e.stopPropagation()}
            oninput={(e) => (renameDraft = e.currentTarget.value)}
            onkeydown={(e) => {
              if (e.key === "Enter") commitRename();
              if (e.key === "Escape") renameId = null;
            }}
            onblur={commitRename}
          />
        {:else}
          <button
            class="maptab mono"
            class:on={launch.activeMapId === m.id}
            role="tab"
            type="button"
            aria-selected={launch.activeMapId === m.id}
            title={m.driveClipIds.length ? `${m.driveClipIds.length} clip${m.driveClipIds.length === 1 ? "" : "s"} use this launcher` : "No clips use this launcher"}
            ondblclick={(e) => {
              e.stopPropagation();
              startRename(m.id, m.name);
            }}
            onclick={() => launch.selectMap(m.id)}
          >{m.name}</button>
        {/if}
      {/each}
      <button
        class="maptab add mono"
        type="button"
        title="New launcher"
        aria-label="New launcher"
        onclick={() => void launch.createMap()}>+</button
      >
      <button
        class="maptab danger mono"
        type="button"
        title="Delete this launcher"
        aria-label="Delete this launcher"
        disabled={launch.maps.length <= 1}
        onclick={() => void launch.removeMap(launch.activeMap.id)}>×</button
      >
    </div>

    <div class="hint silk">
      {#if launch.overlay}
        playing {launch.overlay.name} — arrangement loop and playhead stay on the clip
      {:else if launch.marking}
        MARK is on — drag across any lanes (clips will not move)
      {:else}
        Click a scene name to play it · F2 to rename · GATE/ONE-SHOT below
      {/if}
    </div>

    <div class="list" role="table" aria-label="Launch bindings">
      <div class="headrow silk" role="row">
        <span>name</span>
        <span>note</span>
        <span>ch</span>
        <span>target</span>
        <span></span>
      </div>
      {#if launch.bindings.length === 0}
        <div class="empty silk">no markings yet — press MARK and drag across lanes</div>
      {/if}
      {#each launch.bindings as b (b.id)}
        <div
          class="row"
          class:on={launch.selectedId === b.id}
          class:learn={launch.learningId === b.id}
          class:play={launch.overlay?.id === b.id}
          role="row"
          tabindex="0"
          title="Click the name to play · F2 to rename · click target to jump"
          onclick={() => onRowClick(b.id)}
          ondblclick={() => onRowDblClick(b.id)}
          onkeydown={(e) => {
            if (e.key === "Enter") playScene(b.id);
          }}
        >
          {#if editNameId === b.id}
            <input
              class="name mono"
              value={editNameDraft}
              aria-label="Binding name"
              onpointerdown={(e) => e.stopPropagation()}
              oninput={(e) => (editNameDraft = e.currentTarget.value)}
              onkeydown={(e) => {
                if (e.key === "Enter") commitNameEdit();
                if (e.key === "Escape") editNameId = null;
              }}
              onblur={commitNameEdit}
            />
          {:else}
            <button
              type="button"
              class="name playlab mono"
              title="Play {b.name}"
              onclick={(e) => {
                e.stopPropagation();
                playScene(b.id);
              }}>{b.name}</button
            >
          {/if}
          <select
            class="note mono"
            value={b.note}
            aria-label="MIDI note"
            onpointerdown={(e) => e.stopPropagation()}
            ondblclick={(e) => e.stopPropagation()}
            onchange={(e) => onNoteChange(b.id, e.currentTarget.value)}
          >
            {#each MIDI_NOTES as n (n.note)}
              <option value={n.note}>{n.name}</option>
            {/each}
          </select>
          <select
            class="ch mono"
            value={b.channel ?? ""}
            aria-label="MIDI channel"
            onpointerdown={(e) => e.stopPropagation()}
            onchange={(e) => onChannelChange(b.id, e.currentTarget.value)}
          >
            <option value="">any</option>
            {#each Array.from({ length: 16 }, (_, i) => i) as ch (ch)}
              <option value={ch}>{ch + 1}</option>
            {/each}
          </select>
          <button
            type="button"
            class="tgt silk"
            title="Jump the playhead to this marking"
            onpointerdown={(e) => e.stopPropagation()}
            ondblclick={(e) => e.stopPropagation()}
            onclick={(e) => {
              e.stopPropagation();
              launch.focus(b.id);
            }}>{targetLabel(b)}</button
          >
          <div class="acts">
            <button
              class="mini mono"
              class:on={launch.learningId === b.id}
              title="Learn — next incoming MIDI note binds here"
              onclick={(e) => {
                e.stopPropagation();
                launch.beginLearn(b.id);
              }}>{launch.learningId === b.id ? "…" : "LEARN"}</button
            >
            <button
              class="mini danger mono"
              title="Delete marking"
              onclick={(e) => {
                e.stopPropagation();
                void launch.remove(b.id);
              }}>×</button
            >
          </div>
        </div>
        {#if launch.clipSelfTriggers(b)}
          <div class="warn silk">
            {midiNoteName(b.note)} is also in this clip — AURA will ignore that note when the clip plays itself
          </div>
        {/if}
      {/each}
    </div>

    <div class="foot">
      <button
        class="act mono"
        class:on={(launch.activeMap.playMode ?? "gate") === "gate"}
        title="Gate — the scene sounds only while the MIDI note is held"
        onclick={() => void launch.setPlayMode("gate")}
      >GATE</button
      >
      <button
        class="act mono"
        class:on={(launch.activeMap.playMode ?? "gate") === "oneShot"}
        title="One-shot — the note triggers the scene, which then plays to its end"
        onclick={() => void launch.setPlayMode("oneShot")}
      >ONE-SHOT</button
      >
      <button
        class="act mono"
        class:on={launch.marking}
        aria-pressed={launch.marking}
        title={launch.marking
          ? "MARK is on — drag across lanes to draw a region. Click to turn off."
          : "MARK is off — click to draw a new region (clips will not move)."}
        onclick={() => launch.toggleMarking()}
      >{launch.marking ? "MARK ON" : "MARK OFF"}</button
      >
      <button
        class="act mono"
        disabled={!midi.selectedClipId}
        title="Selected clip's notes fire this launcher"
        onclick={() => void useSelectedClip()}
      >{midi.selectedClipId && launch.driveMap(midi.selectedClipId)?.id === launch.activeMap.id
        ? "USING THIS"
        : "USE ON CLIP"}</button
      >
      <button
        class="act mono"
        disabled={!midi.selectedClipId}
        title="Map the selected MIDI clip to a free note (hardware launches the clip)"
        onclick={() => void mapSelectedClip()}>MAP CLIP</button
      >
      <span class="footnote silk">
        {launch.learningId ? "play a note on your controller…" : launch.marking ? "drag across lanes…" : "MARK to add · drag a marking to move"}
      </span>
    </div>
    {#if launch.error}
      <div class="err silk">{launch.error}</div>
    {/if}
  </div>
{/if}

<style>
  .win {
    position: fixed;
    z-index: 40;
    width: 560px;
    max-height: min(70vh, 640px);
    display: flex;
    flex-direction: column;
    background: rgba(8, 10, 19, 0.94);
    border: 1px solid var(--glass-border);
    box-shadow: 0 18px 48px rgba(0, 0, 0, 0.45);
  }
  .title {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 10px 6px;
    cursor: grab;
    border-bottom: 1px solid var(--glass-border);
  }
  .title:active {
    cursor: grabbing;
  }
  .titlelab {
    font-size: 11px;
    letter-spacing: 0.18em;
    color: var(--cyan);
  }
  .sub {
    flex: 1;
  }
  .x {
    position: relative;
    z-index: 1;
    width: 28px;
    height: 28px;
    font-size: 18px;
    color: var(--text-dim);
    background: transparent;
    border: 0;
    cursor: pointer;
    flex: none;
  }
  .x:hover {
    color: var(--text);
  }
  .maps {
    display: flex;
    flex-wrap: wrap;
    gap: 4px;
    padding: 8px 10px 0;
    align-items: center;
  }
  .maptab {
    font-size: 9px;
    letter-spacing: 0.1em;
    padding: 4px 8px;
    background: transparent;
    border: 1px solid var(--glass-border);
    color: var(--text-dim);
    cursor: pointer;
  }
  .maptab.on {
    color: var(--cyan);
    border-color: var(--cyan-dim);
    background: rgba(82, 229, 255, 0.08);
  }
  .maptab.add,
  .maptab.danger {
    width: 26px;
    padding: 4px 0;
  }
  .maptab.danger:disabled {
    opacity: 0.3;
    cursor: default;
  }
  .maptab.danger:not(:disabled):hover {
    color: var(--red);
    border-color: var(--red);
  }
  .mapname {
    width: 9em;
    background: var(--bg-2);
    border: 0;
    color: var(--text);
    font: inherit;
    font-size: 11px;
    letter-spacing: 0;
    padding: 0;
  }
  .hint {
    padding: 8px 12px 4px;
    color: var(--text-dim);
  }
  .list {
    flex: 1;
    min-height: 80px;
    overflow-y: auto;
    padding: 4px 8px 8px;
  }
  .headrow,
  .row {
    display: grid;
    grid-template-columns: 1fr 72px 52px 1.4fr auto;
    gap: 6px;
    align-items: center;
  }
  .headrow {
    padding: 4px 6px;
    color: var(--text-faint);
  }
  .row {
    padding: 4px 6px;
    border: 1px solid transparent;
    cursor: pointer;
  }
  .row:hover {
    background: rgba(82, 229, 255, 0.04);
  }
  .row.on {
    border-color: var(--cyan-dim);
    background: rgba(82, 229, 255, 0.08);
  }
  .row.learn {
    border-color: var(--amber);
  }
  .row.play {
    border-color: var(--cyan);
    background: rgba(82, 229, 255, 0.1);
  }
  .name,
  .note,
  .ch {
    background: var(--bg-2);
    border: 1px solid var(--glass-border);
    color: var(--text);
    font-size: 11px;
    padding: 3px 5px;
    width: 100%;
  }
  button.name.playlab {
    text-align: left;
    cursor: pointer;
  }
  button.name.playlab:hover {
    color: var(--cyan);
    border-color: var(--cyan-dim);
  }
  .tgt {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--text-dim);
    background: transparent;
    border: 0;
    padding: 0;
    text-align: left;
    font: inherit;
    cursor: pointer;
  }
  .tgt:hover {
    color: var(--cyan);
  }
  .acts {
    display: flex;
    gap: 4px;
  }
  .mini {
    font-size: 8px;
    letter-spacing: 0.12em;
    padding: 3px 6px;
    background: transparent;
    border: 1px solid var(--glass-border);
    color: var(--text-dim);
    cursor: pointer;
  }
  .mini.on {
    color: var(--amber);
    border-color: var(--amber);
  }
  .mini.danger:hover {
    color: var(--red);
    border-color: var(--red);
  }
  .empty {
    padding: 18px 8px;
    text-align: center;
  }
  .warn {
    padding: 0 8px 8px 6px;
    color: var(--amber);
  }
  .foot {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 8px 10px 10px;
    border-top: 1px solid var(--glass-border);
  }
  .act {
    font-size: 9px;
    letter-spacing: 0.14em;
    padding: 6px 10px;
    background: transparent;
    border: 1px solid var(--cyan-dim);
    color: var(--cyan);
    cursor: pointer;
  }
  .act.on {
    color: #1a1408;
    border-color: var(--amber);
    background: var(--amber);
  }
  .act:disabled {
    opacity: 0.35;
    cursor: default;
  }
  .footnote {
    flex: 1;
  }
  .err {
    padding: 0 12px 10px;
    color: var(--red);
  }
</style>
