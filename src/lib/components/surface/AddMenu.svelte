<script lang="ts">
  import { ADD_MENU, type AddMenuId } from "../../utils/control-surface";
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
</script>

<div class="wrap">
  <button
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
    <div class="menu glass" role="menu" tabindex="-1" onkeydown={onKey}>
      {#each ["widget", "recipe", "template"] as group (group)}
        <p class="silk heading">{group === "widget" ? "ADD" : group === "recipe" ? "FILL" : "TEMPLATE"}</p>
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
    position: absolute;
    top: calc(100% + 6px);
    left: 0;
    z-index: 40;
    min-width: 260px;
    max-height: 70vh;
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
