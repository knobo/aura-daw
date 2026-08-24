<script lang="ts">
  /**
   * The audition toggle, in the toolbar rather than only in Preferences: a
   * dialog is the wrong distance away for a setting whose right answer
   * changes every few minutes (design §8.2).
   *
   * It writes the preference itself — there is no second session-level
   * mute that could disagree with it (ruling R-2).
   */
  import { audition } from "../../state/audition.svelte";

  const on = $derived(audition.enabled);
</script>

<button
  type="button"
  class="chip mono"
  class:on
  aria-pressed={on}
  aria-label={on ? "Audition on double-click: on" : "Audition on double-click: off"}
  title={on
    ? "Double-click a row to hear it — click to mute"
    : "Auditioning is off — click to hear rows on double-click"}
  onclick={() => (audition.enabled = !on)}
>
  <span aria-hidden="true">{on ? "♪" : "♪̸"}</span>
</button>

<style>
  .chip {
    flex: none;
    height: 24px;
    min-width: 24px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    padding: 0 6px;
    font-size: 11px;
    line-height: 1;
    border-radius: 5px;
    border: var(--border-width) solid var(--glass-border);
    background: rgb(var(--bg-0-rgb) / 0.7);
    color: var(--text-faint);
    cursor: pointer;
  }
  .chip:hover {
    color: var(--cyan);
    border-color: var(--cyan-dim);
  }
  .chip.on {
    color: var(--cyan);
    border-color: var(--cyan-dim);
    background: rgb(var(--cyan-rgb) / 0.08);
  }
</style>
