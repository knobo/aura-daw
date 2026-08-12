<script lang="ts">
  /**
   * The confirm-in-UI flow (ARCHITECTURE §12.3): when an MCP agent's
   * destructive tool call parks, this dialog takes the screen — unmistakable
   * amber hazard chrome, tool name + args summary, approve/deny, and a
   * draining 60 s fuse (server times out to deny).
   */
  import { mcp } from "../../state/mcp.svelte";

  const current = $derived(mcp.current);

  // fuse: fraction of the 60 s window remaining
  let fuse = $state(1);
  let secondsLeft = $state(60);
  $effect(() => {
    const c = current;
    if (!c) return;
    const started = c.requestedAt ? Date.parse(c.requestedAt) : Date.now();
    let raf = 0;
    const tick = () => {
      raf = requestAnimationFrame(tick);
      const left = 1 - (Date.now() - started) / 60_000;
      fuse = Math.max(0, left);
      secondsLeft = Math.max(0, Math.ceil(left * 60));
    };
    raf = requestAnimationFrame(tick);
    return () => cancelAnimationFrame(raf);
  });

  function onKeydown(e: KeyboardEvent) {
    if (!current) return;
    if (e.key === "Escape") {
      e.preventDefault();
      void mcp.resolve(current.id, false);
    }
  }
</script>

<svelte:window onkeydown={onKeydown} />

{#if current}
  <div class="veil" role="presentation">
    <div class="dialog" role="alertdialog" aria-modal="true" aria-labelledby="mcp-confirm-title">
      <div class="stripe"></div>
      <header class="head">
        <span class="siren"></span>
        <span id="mcp-confirm-title" class="title mono">AGENT REQUESTS ACTION</span>
        <span class="fusetime mono" class:low={secondsLeft <= 10}>{secondsLeft}s</span>
      </header>

      <div class="toolrow">
        <span class="silk">tool</span>
        <span class="tool mono">{current.tool}</span>
      </div>
      <p class="summary">{current.summary}</p>

      {#if mcp.pending.length > 1}
        <div class="queue silk">+{mcp.pending.length - 1} more waiting</div>
      {/if}

      <div class="fuse"><div class="burn" style:width="{fuse * 100}%"></div></div>

      <div class="actions">
        <button class="deny mono" onclick={() => mcp.resolve(current.id, false)}>DENY · ESC</button>
        <button class="approve mono" onclick={() => mcp.resolve(current.id, true)}>APPROVE &amp; RUN</button>
      </div>
      <div class="note silk">no answer in {secondsLeft}s ⇒ denied automatically</div>
    </div>
  </div>
{/if}

<style>
  .veil {
    position: fixed;
    inset: 0;
    z-index: 100;
    display: grid;
    place-items: center;
    background: rgba(5, 7, 13, 0.72);
    backdrop-filter: blur(6px) saturate(0.8);
    -webkit-backdrop-filter: blur(6px) saturate(0.8);
    animation: veil-in 160ms ease-out;
  }
  @keyframes veil-in {
    from {
      opacity: 0;
    }
  }

  .dialog {
    position: relative;
    width: min(480px, calc(100vw - 48px));
    border: 1px solid rgba(255, 200, 87, 0.55);
    border-radius: 8px;
    background: rgba(13, 17, 30, 0.96);
    box-shadow:
      0 0 0 1px rgba(255, 200, 87, 0.15),
      0 0 40px rgba(255, 200, 87, 0.18),
      0 24px 60px rgba(0, 0, 0, 0.6);
    padding: 16px 18px 14px;
    overflow: hidden;
    animation: dialog-in 180ms cubic-bezier(0.2, 0.9, 0.3, 1.2);
  }
  @keyframes dialog-in {
    from {
      opacity: 0;
      transform: translateY(10px) scale(0.97);
    }
  }
  /* hazard tape along the top edge */
  .stripe {
    position: absolute;
    top: 0;
    left: 0;
    right: 0;
    height: 4px;
    background: repeating-linear-gradient(
      -45deg,
      var(--amber) 0 10px,
      rgba(5, 7, 13, 0.9) 10px 20px
    );
    opacity: 0.85;
  }

  .head {
    display: flex;
    align-items: center;
    gap: 10px;
    margin: 4px 0 12px;
  }
  .siren {
    width: 10px;
    height: 10px;
    border-radius: 50%;
    background: var(--amber);
    box-shadow: 0 0 12px rgba(255, 200, 87, 0.9);
    animation: siren 1s ease-in-out infinite;
  }
  @keyframes siren {
    50% {
      box-shadow: 0 0 3px rgba(255, 200, 87, 0.4);
      opacity: 0.7;
    }
  }
  .title {
    flex: 1;
    font-size: 13px;
    font-weight: 600;
    letter-spacing: 0.22em;
    color: var(--amber);
  }
  .fusetime {
    font-size: 13px;
    color: var(--text-dim);
  }
  .fusetime.low {
    color: var(--red);
  }

  .toolrow {
    display: flex;
    align-items: center;
    gap: 8px;
    margin-bottom: 6px;
  }
  .tool {
    font-size: 12px;
    color: var(--cyan);
    background: rgba(82, 229, 255, 0.07);
    border: 1px solid rgba(82, 229, 255, 0.25);
    border-radius: 4px;
    padding: 2px 8px;
  }
  .summary {
    margin: 0 0 10px;
    font-size: 13px;
    line-height: 1.5;
    color: var(--text);
    word-break: break-word;
  }
  .queue {
    margin-bottom: 8px;
    color: var(--amber);
  }

  .fuse {
    height: 3px;
    border-radius: 2px;
    background: rgba(96, 130, 190, 0.18);
    overflow: hidden;
    margin-bottom: 12px;
  }
  .burn {
    height: 100%;
    background: linear-gradient(90deg, var(--red), var(--amber));
    box-shadow: 0 0 8px rgba(255, 200, 87, 0.6);
  }

  .actions {
    display: flex;
    gap: 10px;
  }
  .deny,
  .approve {
    flex: 1;
    padding: 9px 0;
    font-size: 10px;
    letter-spacing: 0.18em;
    border-radius: 5px;
    cursor: pointer;
    transition: filter 120ms, box-shadow 120ms;
  }
  .deny {
    border: 1px solid rgba(255, 65, 82, 0.5);
    background: rgba(255, 65, 82, 0.08);
    color: var(--red);
  }
  .deny:hover {
    background: rgba(255, 65, 82, 0.18);
  }
  .approve {
    border: none;
    background: linear-gradient(100deg, var(--amber), #ff8b5c);
    color: var(--bg-0);
    box-shadow: 0 0 16px rgba(255, 200, 87, 0.35);
  }
  .approve:hover {
    filter: brightness(1.12);
  }

  .note {
    margin-top: 9px;
    text-align: center;
  }
</style>
