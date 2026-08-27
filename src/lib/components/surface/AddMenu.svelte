<script lang="ts">
  /**
   * The `+` dropdown. The device racks live one level down rather than in
   * the root list: with four devices as data — and the list meant to grow —
   * a flat menu is seventeen rows against the bottom edge of the window.
   *
   * A drill-down, not a side-opening flyout: this popover is already
   * height-capped against that same edge (see `popover.ts`), and a second
   * one hanging off its side would have to fight for the same room. The
   * box re-places itself when the content height changes, so swapping the
   * level is free.
   */
  import { ADD_MENU, DEVICE_RACKS, type AddMenuId } from "../../utils/control-surface";
  import { popoverBox } from "../../utils/popover.svelte";
  import { portal } from "../../utils/portal";
  import { surface } from "../../state/surface.svelte";

  let level = $state<"root" | "rack">("root");

  const racks = ADD_MENU.filter((i) => i.group === "rack");
  const rackHint = DEVICE_RACKS.map((d) => d.name).join(" · ");

  function toggleMenu() {
    level = "root";
    surface.addOpen = !surface.addOpen;
  }

  function pick(id: AddMenuId) {
    surface.choose(id);
    level = "root";
  }

  function go(next: "root" | "rack") {
    level = next;
    box.node?.focus();
  }

  function onKey(e: KeyboardEvent) {
    if (e.key !== "Escape") return;
    e.stopPropagation();
    // Escape peels one level before it closes the menu.
    if (level === "rack") {
      go("root");
      return;
    }
    surface.addOpen = false;
  }

  // The panel is docked at the bottom of the window, so this menu opens
  // upward — and it is tall enough that "upward" can overshoot the top too.
  // `popoverBox` picks the side with room and caps the scroll box, so the
  // rows at the end of the list are always reachable.
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
    onclick={toggleMenu}
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
      {#if level === "root"}
        {#each ["widget", "recipe"] as group (group)}
          <p class="silk heading">{group === "widget" ? "ADD" : "FILL"}</p>
          {#each ADD_MENU.filter((i) => i.group === group) as item (item.id)}
            <button class="item" role="menuitem" type="button" onclick={() => pick(item.id)}>
              <span class="name">{item.label}</span>
              <span class="silk hint">{item.hint}</span>
            </button>
          {/each}
        {/each}

        <p class="silk heading">RACK</p>
        <button
          class="item drill"
          role="menuitem"
          type="button"
          aria-haspopup="menu"
          onclick={() => go("rack")}
        >
          <span class="name">Add rack<span class="chev">›</span></span>
          <span class="silk hint">{rackHint}</span>
        </button>

        <p class="silk heading">PAGE</p>
        {#each ADD_MENU.filter((i) => i.group === "page") as item (item.id)}
          <button class="item" role="menuitem" type="button" onclick={() => pick(item.id)}>
            <span class="name">{item.label}</span>
            <span class="silk hint">{item.hint}</span>
          </button>
        {/each}
      {:else}
        <button class="back" role="menuitem" type="button" onclick={() => go("root")}>
          <span class="chev back-chev">‹</span>
          <span class="silk">RACK</span>
        </button>
        {#each racks as item (item.id)}
          <button class="item" role="menuitem" type="button" onclick={() => pick(item.id)}>
            <span class="name">{item.label}</span>
            <span class="silk hint">{item.hint}</span>
          </button>
        {/each}
      {/if}
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
  .menu:focus-visible {
    outline: var(--focus-width) solid var(--cyan);
    outline-offset: -2px;
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
  .chev {
    margin-left: 6px;
    color: var(--text-faint);
  }
  .back {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 6px 8px;
    border: none;
    background: transparent;
    color: var(--text-dim);
    border-radius: var(--ctrl-radius);
    cursor: pointer;
    text-align: left;
  }
  .back:hover {
    background: rgb(var(--cyan-rgb) / 0.08);
    color: var(--text);
  }
  .back-chev {
    margin-left: 0;
    font-size: 14px;
    line-height: 1;
  }
</style>
