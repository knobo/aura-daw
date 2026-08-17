<script lang="ts">
  /**
   * One row of any library root: glyph, label, dim meta, optional drag.
   * Shared by SAMPLES / CLIPS / PRESETS so the three roots cannot drift apart
   * visually or behaviourally.
   */
  interface Props {
    label: string;
    meta?: string;
    icon?: string;
    active?: boolean;
    draggable?: boolean;
    onclick?: () => void;
    ondragstart?: (e: DragEvent) => void;
  }
  let {
    label,
    meta = "",
    icon = "",
    active = false,
    draggable = false,
    onclick,
    ondragstart,
  }: Props = $props();
</script>

<!-- A div, not a <button>: WebKit refuses to start a drag from a native form
     control even with draggable="true", which silently killed every library
     drag under WebKitGTK (Tauri on Linux). -->
<div
  class="row"
  class:on={active}
  {draggable}
  role="button"
  tabindex="0"
  title={label}
  onclick={() => onclick?.()}
  onkeydown={(e) => {
    if (e.key === "Enter" || e.key === " ") {
      e.preventDefault();
      onclick?.();
    }
  }}
  ondragstart={(e) => ondragstart?.(e)}
>
  {#if icon}<span class="icon" aria-hidden="true">{icon}</span>{/if}
  <span class="label">{label}</span>
  {#if meta}<span class="meta silk">{meta}</span>{/if}
</div>

<style>
  .row {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 5px 6px;
    border: none;
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
    color: var(--cyan);
    background: rgb(var(--cyan-rgb) / 0.08);
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
  .meta {
    flex: none;
    letter-spacing: 0.08em;
  }
</style>
