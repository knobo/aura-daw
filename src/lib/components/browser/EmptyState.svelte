<script lang="ts">
  /**
   * Directive, not moody. An empty list is an invitation to act ("Scan to
   * find your CLAP and LV2 installs"), and an error says what happened and
   * what to do next — in the interface's voice, never an apology.
   */
  let {
    message,
    variant = "empty",
    action,
  }: {
    message: string;
    variant?: "empty" | "error";
    action?: { label: string; onclick: () => void };
  } = $props();
</script>

<div class="state silk" class:error={variant === "error"} role={variant === "error" ? "alert" : undefined}>
  <p class="msg">{message}</p>
  {#if action}
    <button type="button" class="act mono" onclick={action.onclick}>{action.label}</button>
  {/if}
</div>

<style>
  .state {
    padding: 14px 4px;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 8px;
  }
  .msg {
    letter-spacing: 0.08em;
    line-height: 1.5;
  }
  .error .msg {
    color: var(--red);
  }
  .act {
    padding: 5px 10px;
    font-size: 9px;
    letter-spacing: 0.14em;
    border-radius: 4px;
    border: var(--border-width) solid rgb(var(--cyan-rgb) / 0.35);
    background: rgb(var(--cyan-rgb) / 0.07);
    color: var(--cyan);
    cursor: pointer;
  }
  .act:hover {
    background: rgb(var(--cyan-rgb) / 0.16);
  }
</style>
