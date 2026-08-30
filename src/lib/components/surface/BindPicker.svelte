<script lang="ts">
  /**
   * Bind picker: what a single widget points at. The `+` menu inserts
   * unbound knobs, faders, gauges, pads and lamps; without this the only
   * thing you could do with one was delete it again.
   *
   * The option list is pure (`bindOptions`); this is the popover around it.
   * Root shows one drill tile per group (`CLIP ›`, `LEVEL ›`, …) rather than
   * a flat list — a project with real content runs a flat list into the
   * bottom of the window, the same reason PR 116 (written without a hash,
   * which the theme guard would read as a three-digit colour) made the `+`
   * menu's rack list a drill-down, and the same reason "Add pad from
   * clip ›" was already one. `level` carries either sentinel ("root",
   * "player") or a group name — picking a tile drills into that group.
   */
  import { surface } from "../../state/surface.svelte";
  import { targetKey, type BindOption, type SurfaceTarget, type SurfaceWidget } from "../../utils/control-surface";
  import { popoverBox } from "../../utils/popover.svelte";
  import { portal } from "../../utils/portal";
  import { project } from "../../state/project.svelte";
  import { addAudioPlayer } from "../../state/players.svelte";

  /** `cell` set = the picker is filling one pad-grid slot, not the widget.
   * A cell now holds a full `SurfaceTarget` (same union a loose pad's
   * `target` does), so it gets the same option set — clip, launcher row,
   * track mute, and "Add pad from clip ›". */
  const { widget, cell }: { widget: SurfaceWidget; cell?: number } = $props();

  let level = $state<string>("root");
  let raw = $state(true);
  const canAddPlayerPad = $derived(cell != null || widget.kind === "pad");
  const audioClips = $derived(project.clips);

  function goPlayerLevel() {
    level = "player";
  }
  function goRoot() {
    level = "root";
  }

  async function pickClip(clipId: string, name: string) {
    const id = await addAudioPlayer(clipId, raw);
    const target: SurfaceTarget = { kind: "player", playerId: id };
    if (cell != null) surface.bindCell(widget.id, cell, target);
    else surface.bind(widget.id, target, name);
    level = "root";
  }

  const options = $derived(cell == null ? surface.options(widget.kind) : surface.cellChoices());

  /** Groups in first-seen order — the tiles the root level drills into. */
  const groups = $derived(
    options.reduce<string[]>((acc, o) => (acc.includes(o.group) ? acc : [...acc, o.group]), []),
  );

  function cellTarget(index: number): SurfaceTarget | null {
    return widget.cells?.[index] ?? null;
  }

  const current = $derived(cell == null ? targetKey(widget.target) : targetKey(cellTarget(cell)));

  function isCurrent(o: BindOption): boolean {
    if (current === null || targetKey(o.target) !== current) return false;
    return cell != null || !o.lampRole || o.lampRole === widget.lampRole;
  }

  function pick(o: BindOption) {
    if (cell != null) {
      surface.bindCell(widget.id, cell, o.target);
      return;
    }
    surface.bind(widget.id, o.target, o.widgetLabel, o.lampRole ? { lampRole: o.lampRole } : undefined);
  }

  function clear() {
    if (cell != null) surface.bindCell(widget.id, cell, null);
    else surface.bind(widget.id, null);
  }

  const canClear = $derived(cell == null ? !!widget.target : cellTarget(cell) !== null);

  function onKey(e: KeyboardEvent) {
    if (e.key !== "Escape") return;
    e.stopPropagation();
    if (level !== "root") {
      goRoot();
      return;
    }
    surface.bindFor = null;
  }

  // Placed `fixed` against the widget's own box: the deck scrolls
  // (`overflow: auto`) and is docked at the bottom of the window, so an
  // absolutely positioned popover is clipped by one edge or the other
  // whichever way it is anchored.
  // The marker stays behind in the widget's cell after the popover is
  // portaled out; the cell around it is what we hang off.
  let marker: HTMLElement | undefined = $state();
  const anchor = () => marker?.parentElement;
  const box = popoverBox(anchor);

  $effect(() => {
    // Anything outside the popover closes it — including the next widget's
    // own bind button, which then opens its own.
    const off = (e: PointerEvent) => {
      const el = box.node;
      const target = e.target as Node;
      // The trigger lives in the anchor cell; let its own click do the toggle.
      if (!el || el.contains(target) || anchor()?.contains(target)) return;
      surface.bindFor = null;
    };
    window.addEventListener("pointerdown", off);
    return () => window.removeEventListener("pointerdown", off);
  });
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<span bind:this={marker} class="anchor" aria-hidden="true"></span>
<div
  bind:this={box.node}
  use:portal
  class="picker glass"
  style:left={box.placement ? `${box.placement.left}px` : undefined}
  style:top={box.placement ? `${box.placement.top}px` : undefined}
  style:max-height={box.placement ? `${box.placement.maxHeight}px` : undefined}
  style:visibility={box.placement ? undefined : "hidden"}
  role="menu"
  tabindex="-1"
  aria-label={cell == null ? `Bind ${widget.label}` : `Assign pad ${cell + 1}`}
  onkeydown={onKey}
>
  {#if level === "player"}
    <button class="back" role="menuitem" type="button" onclick={goRoot}>
      <span class="chev back-chev">‹</span>
      <span class="silk">PLAYER</span>
    </button>
    <label class="raw-toggle">
      <input type="checkbox" bind:checked={raw} />
      <span class="silk">raw (V-6: unity, no chain)</span>
    </label>
    {#if audioClips.length === 0}
      <p class="silk empty">This project has no audio clips yet.</p>
    {:else}
      {#each audioClips as c (c.id)}
        <button class="item" role="menuitem" type="button" onclick={() => pickClip(c.id, c.name)}>
          {c.name}
        </button>
      {/each}
    {/if}
  {:else if level === "root"}
    {#if options.length === 0 && !canAddPlayerPad}
      <p class="silk empty">Nothing to bind yet — this project has no matching target.</p>
    {:else}
      {#each groups as g (g)}
        <button class="item drill" role="menuitem" type="button" aria-haspopup="menu" onclick={() => (level = g)}>
          {g} <span class="chev">›</span>
        </button>
      {/each}
    {/if}
    {#if canAddPlayerPad}
      <button class="item drill" role="menuitem" type="button" aria-haspopup="menu" onclick={goPlayerLevel}>
        Add pad from clip<span class="chev">›</span>
      </button>
    {/if}
  {:else}
    <button class="back" role="menuitem" type="button" onclick={goRoot}>
      <span class="chev back-chev">‹</span>
      <span class="silk">{level}</span>
    </button>
    {#each options.filter((o) => o.group === level) as o (o.key)}
      <button
        class="item"
        class:on={isCurrent(o)}
        role="menuitem"
        type="button"
        onclick={() => pick(o)}
      >
        {o.label}
      </button>
    {/each}
  {/if}
  {#if level === "root" && canClear}
    <button class="item clear" role="menuitem" type="button" onclick={clear}>
      {cell == null ? "Unbind" : "Clear pad"}
    </button>
  {/if}
</div>

<style>
  .anchor {
    display: none;
  }
  .picker {
    position: fixed;
    z-index: 60;
    min-width: 180px;
    max-height: 260px; /* replaced by the measured cap once placed */
    overflow: auto;
    padding: 6px;
    border-radius: var(--ctrl-radius);
    box-shadow: var(--bevel-raised), var(--relief-2);
    text-align: left;
  }
  .item {
    display: block;
    width: 100%;
    padding: 4px 6px;
    border: none;
    border-radius: calc(var(--ctrl-radius) - 2px);
    background: transparent;
    color: var(--text);
    font-size: 11px;
    text-align: left;
    cursor: pointer;
    white-space: nowrap;
  }
  .item:hover,
  .item:focus-visible {
    background: rgb(var(--line-rgb) / 0.12);
  }
  .item.on {
    color: var(--cyan);
  }
  .clear {
    margin-top: 4px;
    border-top: var(--border-width) solid var(--glass-border);
    color: var(--text-dim);
  }
  .empty {
    padding: 6px;
    color: var(--text-dim);
    max-width: 190px;
    white-space: normal;
  }
  .drill {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  .chev {
    margin-left: 6px;
    color: var(--text-faint);
  }
  .back {
    display: flex;
    align-items: center;
    gap: 6px;
    width: 100%;
    padding: 4px 6px;
    border: none;
    background: transparent;
    color: var(--text-dim);
    border-radius: calc(var(--ctrl-radius) - 2px);
    cursor: pointer;
    text-align: left;
  }
  .back:hover {
    background: rgb(var(--line-rgb) / 0.12);
    color: var(--text);
  }
  .back-chev {
    font-size: 14px;
    line-height: 1;
  }
  .raw-toggle {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 4px 6px;
    color: var(--text-dim);
    font-size: 11px;
    cursor: pointer;
  }
</style>
