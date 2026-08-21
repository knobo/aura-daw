<script lang="ts">
  /** The star. A row's favourite toggle — `aria-pressed` carries the
   * state, the title names the action rather than describing the icon. */
  let {
    favorite,
    itemLabel = "",
    onclick,
  }: {
    favorite: boolean;
    /** What this star favourites, for the title ("Add Surge XT to favourites"). */
    itemLabel?: string;
    onclick?: () => void;
  } = $props();

  const subject = $derived(itemLabel ? ` ${itemLabel}` : "");
  const title = $derived(
    favorite ? `Remove${subject} from favourites` : `Add${subject} to favourites`,
  );
</script>

<button
  type="button"
  class="star"
  class:on={favorite}
  aria-pressed={favorite}
  {title}
  onclick={(e) => {
    // Stars sit inside a clickable row — don't also fire the row's
    // primary action.
    e.stopPropagation();
    onclick?.();
  }}
>
  {favorite ? "★" : "☆"}
</button>

<style>
  .star {
    flex: none;
    width: 18px;
    height: 18px;
    line-height: 1;
    display: flex;
    align-items: center;
    justify-content: center;
    padding: 0;
    border: none;
    border-radius: 3px;
    background: transparent;
    color: var(--text-faint);
    font-size: 12px;
    cursor: pointer;
  }
  .star:hover {
    color: var(--amber);
  }
  .star.on {
    color: var(--amber);
  }
</style>
