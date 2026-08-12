<script lang="ts">
  /**
   * Toast stack (bottom-right): export completions, import/stem landings,
   * loop-jam swaps. Glass cards on the house palette; auto-expire.
   */
  import { toasts } from "../state/toasts.svelte";
</script>

{#if toasts.list.length}
  <div class="stack" role="status" aria-live="polite">
    {#each toasts.list as t (t.id)}
      <div class="toast glass {t.kind}">
        <div class="head">
          <span class="dot"></span>
          <span class="title mono">{t.title}</span>
          <button class="x mono" title="Dismiss" onclick={() => toasts.dismiss(t.id)}>×</button>
        </div>
        {#each t.lines as line, i (i)}
          <div class="line" class:mono={line.startsWith("/")}>{line}</div>
        {/each}
      </div>
    {/each}
  </div>
{/if}

<style>
  .stack {
    position: fixed;
    right: 14px;
    bottom: 46px;
    z-index: 90;
    display: flex;
    flex-direction: column;
    gap: 8px;
    max-width: 380px;
    pointer-events: none;
  }
  .toast {
    pointer-events: auto;
    border-radius: 7px;
    padding: 9px 11px;
    background: rgba(8, 10, 19, 0.92);
    animation: toast-in 200ms ease-out;
  }
  @keyframes toast-in {
    from {
      transform: translateY(10px);
      opacity: 0;
    }
  }
  .toast.success {
    border-color: rgba(92, 242, 184, 0.35);
    box-shadow: 0 0 18px rgba(92, 242, 184, 0.12);
  }
  .toast.error {
    border-color: rgba(255, 65, 82, 0.4);
    box-shadow: 0 0 18px rgba(255, 65, 82, 0.12);
  }
  .toast.info {
    border-color: var(--cyan-dim);
    box-shadow: 0 0 18px rgba(82, 229, 255, 0.1);
  }

  .head {
    display: flex;
    align-items: center;
    gap: 7px;
  }
  .dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    flex: none;
  }
  .success .dot {
    background: #5cf2b8;
    box-shadow: 0 0 6px rgba(92, 242, 184, 0.8);
  }
  .error .dot {
    background: var(--red);
    box-shadow: 0 0 6px rgba(255, 65, 82, 0.8);
  }
  .info .dot {
    background: var(--cyan);
    box-shadow: 0 0 6px var(--cyan-dim);
  }
  .title {
    flex: 1;
    font-size: 9px;
    letter-spacing: 0.18em;
    color: var(--text);
  }
  .success .title {
    color: #5cf2b8;
  }
  .error .title {
    color: var(--red);
  }
  .info .title {
    color: var(--cyan);
  }
  .x {
    border: none;
    background: transparent;
    color: var(--text-faint);
    cursor: pointer;
    font-size: 12px;
    padding: 0 2px;
  }
  .x:hover {
    color: var(--red);
  }

  .line {
    margin-top: 4px;
    font-size: 10px;
    color: var(--text-dim);
    word-break: break-all;
  }
  .line.mono {
    font-size: 9px;
  }
</style>
