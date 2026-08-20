<script lang="ts">
  /**
   * Test-only harness: a minimal two-group tree wired onto `BrowserShell`,
   * so `browser-shell.dom.test.ts` can drive real keyboard/DOM behaviour
   * instead of re-testing `browser-model.ts`'s pure functions through a
   * mount. Not part of the public component API.
   */
  import { type BrowserGroup, type BrowserRowRef, flattenRows, rowId } from "./browser-model";
  import BrowserShell from "./BrowserShell.svelte";
  import BrowserSection from "./BrowserSection.svelte";
  import BrowserRow from "./BrowserRow.svelte";

  let {
    onActivate,
    onToggleGroup,
  }: {
    onActivate?: (row: BrowserRowRef) => void;
    onToggleGroup?: (groupKey: string, expand: boolean) => void;
  } = $props();

  const groups: BrowserGroup<string>[] = [
    { key: "g1", label: "Group 1", items: ["Alpha", "Bravo"] },
    { key: "g2", label: "Group 2", items: ["Charlie"] },
  ];

  let query = $state("");
  let collapsed = $state(new Set<string>());
  const rows = $derived(flattenRows(groups, collapsed));

  function toggle(key: string, expand: boolean) {
    const next = new Set(collapsed);
    if (expand) next.delete(key);
    else next.add(key);
    collapsed = next;
    onToggleGroup?.(key, expand);
  }

  function activate(row: BrowserRowRef) {
    onActivate?.(row);
  }
</script>

<BrowserShell bind:query status="{groups.reduce((n, g) => n + g.items.length, 0)} things" {rows} onToggleGroup={toggle} onActivate={activate}>
  {#snippet children({ activeId, setActive })}
    {#each groups as g (g.key)}
      <BrowserSection
        id={rowId({ kind: "group", groupKey: g.key, itemIndex: -1 })}
        label={g.label}
        count={g.items.length}
        expanded={!collapsed.has(g.key)}
        active={activeId === rowId({ kind: "group", groupKey: g.key, itemIndex: -1 })}
        onToggle={() => {
          setActive({ kind: "group", groupKey: g.key, itemIndex: -1 });
          toggle(g.key, collapsed.has(g.key));
        }}
      >
        {#each g.items as item, i (item)}
          <BrowserRow
            id={rowId({ kind: "item", groupKey: g.key, itemIndex: i })}
            label={item}
            active={activeId === rowId({ kind: "item", groupKey: g.key, itemIndex: i })}
            onclick={() => {
              setActive({ kind: "item", groupKey: g.key, itemIndex: i });
              activate({ kind: "item", groupKey: g.key, itemIndex: i });
            }}
          />
        {/each}
      </BrowserSection>
    {/each}
  {/snippet}
</BrowserShell>
