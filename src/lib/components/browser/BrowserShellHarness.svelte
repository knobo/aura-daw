<script lang="ts">
  /**
   * Test-only harness: a minimal two-group tree wired onto `BrowserShell`,
   * so `browser-shell.dom.test.ts` can drive real keyboard/DOM behaviour
   * instead of re-testing `browser-model.ts`'s pure functions through a
   * mount. Not part of the public component API.
   *
   * It filters on the query and drives its folds through `FoldState` for
   * the same reason a real browser does — §8.1's "search reveals, clearing
   * restores" only means anything against a browser that actually narrows.
   */
  import { type BrowserGroup, type BrowserRowRef, flattenRows, rankItems, rowId } from "./browser-model";
  import {
    anyCollapsed as anyFolded,
    beginSearch,
    effectiveFolds,
    emptyFoldState,
    endSearch,
    setAllCollapsed,
    toggleFold,
    type FoldState,
  } from "./browser-folds";
  import BrowserShell from "./BrowserShell.svelte";
  import BrowserSection from "./BrowserSection.svelte";
  import BrowserRow from "./BrowserRow.svelte";

  let {
    onActivate,
    onToggleGroup,
    foldAll = true,
  }: {
    onActivate?: (row: BrowserRowRef) => void;
    onToggleGroup?: (groupKey: string, expand: boolean) => void;
    /** Off for the "a browser with no groups gets no button" case. */
    foldAll?: boolean;
  } = $props();

  const ALL: BrowserGroup<string>[] = [
    { key: "g1", label: "Group 1", items: ["Alpha", "Bravo"] },
    { key: "g2", label: "Group 2", items: ["Charlie"] },
  ];

  let query = $state("");
  let folds = $state<FoldState>(emptyFoldState());

  // A query opens the search layer; clearing it puts the user's own folds
  // back. Untracked write into `folds` would be wrong here — this IS the
  // reaction to the query changing.
  $effect(() => {
    folds = query.trim() ? beginSearch(folds) : endSearch(folds);
  });

  const groups = $derived(
    ALL.map((g) => ({ ...g, items: rankItems(g.items, query, [{ value: (s: string) => s }]) })).filter(
      (g) => g.items.length > 0,
    ),
  );
  const collapsed = $derived(effectiveFolds(folds));
  const rows = $derived(flattenRows(groups, collapsed));
  const groupKeys = $derived(groups.map((g) => g.key));

  function toggle(key: string, expand: boolean) {
    if (collapsed.has(key) === expand) folds = toggleFold(folds, key);
    onToggleGroup?.(key, expand);
  }

  function activate(row: BrowserRowRef) {
    onActivate?.(row);
  }
</script>

<BrowserShell
  bind:query
  status="{groups.reduce((n, g) => n + g.items.length, 0)} things"
  {rows}
  onToggleGroup={toggle}
  onActivate={activate}
  anyCollapsed={anyFolded(folds, groupKeys)}
  onFoldAll={foldAll ? (collapse) => (folds = setAllCollapsed(folds, groupKeys, collapse)) : undefined}
>
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
