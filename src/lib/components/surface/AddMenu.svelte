<script lang="ts">
  import { ADD_MENU, type AddMenuId } from "../../utils/control-surface";
  import { popoverBox } from "../../utils/popover.svelte";
  import { portal } from "../../utils/portal";
  import { surface } from "../../state/surface.svelte";

  function pick(id: AddMenuId) {
    surface.choose(id);
  }

  function onKey(e: KeyboardEvent) {
    if (e.key === "Escape") {
      e.stopPropagation();
      surface.addOpen = false;
    }
  }

  // The panel is docked at the bottom of the window, so this menu opens
  // upward — and it is tall enough that "upward" can overshoot the top too.
  // `popoverBox` picks the side with room and caps the scroll box, so the
  // templates at the end of the list are always reachable.
  let plusEl: HTMLElement | undefined = $state();
  const box = popoverBox(() => plusEl);
</script>

<div class="wrap">
  <button
    bind:this={plusEl}
    class="plus raised"
    class:on={surface.addOpen}
    type="button"
    aria-haspopup="menu"
    aria-expanded={surface.addOpen}
    aria-label="Add a control"
    title="Add a control"
    onclick={() => (surface.addOpen = !surface.addOpen)}
  >
    +
  </button>
  {#if surface.addOpen}
    <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
    <div
      bind:this={box.node}
      use:portal
      class="menu glass"
      style:left={box.placement ? `${box.placement.left}px` : undefined}
      style:top={box.placement ? `${box.placement.top}px` : undefined}
      style:max-height={box.placement ? `${box.placement.maxHeight}px` : undefined}
      style:visibility={box.placement ? undefined : "hidden"}
      role="menu"
      tabindex="-1"
      onkeydown={onKey}
    >
      {#each ["widget", "recipe", "rack", "page"] as group (group)}
        <p class="silk heading">
          {group === "widget" ? "ADD" : group === "recipe" ? "FILL" : group === "rack" ? "RACK" : "PAGE"}
        </p>
        {#each ADD_MENU.filter((i) => i.group === group) as item (item.id)}
          <button class="item" role="menuitem" type="button" onclick={() => pick(item.id)}>
            <span class="name">{item.label}</span>
            <span class="silk hint">{item.hint}</span>
          </button>
        {/each}
      {/each}
    </div>
  {/if}
</div>

<style>
  .wrap {
    position: relative;
  }
  .plus {
    width: 28px;
    height: 24px;
    font-size: 16px;
    line-height: 1;
    color: var(--text);
    cursor: pointer;
  }
  .plus.on {
    color: var(--cyan);
  }
  .menu {
    position: fixed;
    z-index: 60;
    min-width: 260px;
    max-height: 70vh; /* replaced by the measured cap once placed */
    overflow: auto;
    padding: 8px;
    display: flex;
    flex-direction: column;
    gap: 2px;
    box-shadow: var(--relief-3);
  }
  .heading {
    padding: 8px 8px 4px;
    color: var(--text-faint);
  }
  .item {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 1px;
    padding: 6px 8px;
    border: none;
    background: transparent;
    color: var(--text);
    border-radius: var(--ctrl-radius);
    cursor: pointer;
    text-align: left;
  }
  .item:hover {
    background: rgb(var(--cyan-rgb) / 0.08);
  }
  .name {
    font-size: 12px;
  }
  .hint {
    color: var(--text-dim);
  }
</style>
