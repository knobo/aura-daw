<script lang="ts">
  /**
   * Bind picker: what a single widget points at. The `+` menu inserts
   * unbound knobs, faders, gauges, pads and lamps; without this the only
   * thing you could do with one was delete it again.
   *
   * The option list is pure (`bindOptions`); this is the popover around it.
   */
  import { surface } from "../../state/surface.svelte";
  import { targetKey, type BindOption, type SurfaceWidget } from "../../utils/control-surface";
  import { popoverBox } from "../../utils/popover.svelte";
  import { portal } from "../../utils/portal";

  /** `cell` set = the picker is filling one pad-grid slot, not the widget. */
  const { widget, cell }: { widget: SurfaceWidget; cell?: number } = $props();

  const options = $derived(cell == null ? surface.options(widget.kind) : surface.cellChoices());
  const current = $derived(
    cell == null
      ? targetKey(widget.target)
      : targetKey(cellClip(cell) ? { kind: "clipLaunch", clipId: cellClip(cell) as string } : null),
  );

  function cellClip(index: number): string | null {
    return widget.cells?.[index] ?? null;
  }

  function isCurrent(o: BindOption): boolean {
    if (current === null || targetKey(o.target) !== current) return false;
    return cell != null || !o.lampRole || o.lampRole === widget.lampRole;
  }

  function pick(o: BindOption) {
    if (cell != null) {
      surface.bindCell(widget.id, cell, o.target.kind === "clipLaunch" ? o.target.clipId : null);
      return;
    }
    surface.bind(widget.id, o.target, o.widgetLabel, o.lampRole ? { lampRole: o.lampRole } : undefined);
  }

  function clear() {
    if (cell != null) surface.bindCell(widget.id, cell, null);
    else surface.bind(widget.id, null);
  }

  const canClear = $derived(cell == null ? !!widget.target : cellClip(cell) !== null);

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.stopPropagation();
      surface.bindFor = null;
    }
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
  {#if options.length === 0}
    <p class="silk empty">Nothing to bind yet — this project has no matching target.</p>
  {:else}
    {#each options as o, i (o.key)}
      {#if i === 0 || options[i - 1].group !== o.group}
        <p class="silk heading">{o.group}</p>
      {/if}
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
  {#if canClear}
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
  .heading {
    padding: 4px 6px 2px;
    color: var(--text-faint);
    font-size: 8px;
    letter-spacing: 0.16em;
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
</style>
