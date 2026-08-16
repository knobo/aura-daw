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
    launch.focus(id);
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

  async function mapSelectedClip() {
    const id = midi.selectedClipId;
    if (!id) return;
    const b = await launch.mapClip(id);
    if (b) launch.focus(b.id);
  }

  function onKeydown(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.stopPropagation();
      if (launch.learningId) launch.stopLearn();
      else if (launch.marking) launch.marking = false;
      else if (launch.selectedId) launch.selectedId = null;
      else launch.closePanel();
      return;
    }
    if ((e.key === "Delete" || e.key === "Backspace") && launch.selectedId) {
      const tag = (e.target as HTMLElement)?.tagName;
      if (tag === "INPUT" || tag === "SELECT" || tag === "TEXTAREA") return;
      e.preventDefault();
      e.stopPropagation();
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
      <span class="sub silk">{launch.bindings.length} marking{launch.bindings.length === 1 ? "" : "s"}</span>
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

    <div class="hint silk">
      {launch.marking
        ? "MARK is on — drag across any lanes (clips will not move)"
        : "MARK to draw a new region · drag a marking to move it · edges resize"}
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
          role="row"
          tabindex="0"
          onclick={() => onRowClick(b.id)}
          onkeydown={(e) => {
            if (e.key === "Enter") onRowClick(b.id);
          }}
        >
          <input
            class="name mono"
            value={b.name}
            aria-label="Binding name"
            onpointerdown={(e) => e.stopPropagation()}
            onchange={(e) => void launch.update(b.id, { name: e.currentTarget.value.trim() || b.name })}
          />
          <select
            class="note mono"
            value={b.note}
            aria-label="MIDI note"
            onpointerdown={(e) => e.stopPropagation()}
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
          <span class="tgt silk" title={targetLabel(b)}>{targetLabel(b)}</span>
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
        title="Map the selected MIDI clip to a free note"
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
    width: 520px;
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
  .tgt {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--text-dim);
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
