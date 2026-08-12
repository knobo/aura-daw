<script lang="ts">
  /**
   * MCP control panel: server status (port, token fingerprint + token-file
   * hint — the token itself never reaches the UI), the policy mode selector,
   * and the agent activity feed (confirm requests and their outcomes).
   */
  import { mcp } from "../../state/mcp.svelte";
  import type { McpPolicyMode } from "../../types/ipc";

  const MODES: { mode: McpPolicyMode; label: string; blurb: string }[] = [
    { mode: "readOnly", label: "READ ONLY", blurb: "agents may inspect, never change" },
    { mode: "confirmDestructive", label: "CONFIRM", blurb: "destructive calls wait for you" },
    { mode: "full", label: "FULL", blurb: "everything runs unattended" },
  ];

  const status = $derived(mcp.status);

  const RES_GLYPH: Record<string, string> = {
    requested: "…",
    approved: "✓",
    denied: "✕",
    expired: "⌛",
  };
</script>

<div class="panel">
  <div class="statusbox">
    <div class="row">
      <span class="led" class:on={status?.running}></span>
      <span class="state mono">{status ? (status.running ? "SERVING" : "OFFLINE") : "…"}</span>
      {#if status?.running}
        <span class="addr mono">127.0.0.1:{status.port}/mcp</span>
      {/if}
    </div>
    {#if status}
      <div class="row token">
        <span class="silk">token</span>
        <span class="fp mono" title="8-char fingerprint — the full token is never shown">{status.tokenFingerprint}…</span>
      </div>
      <div class="hint silk">
        full token (rotates each launch): <span class="mono">~/.local/share/aura/mcp-token</span>
      </div>
    {/if}
  </div>

  <div class="modes" role="radiogroup" aria-label="MCP policy mode">
    <span class="silk">agent policy</span>
    {#each MODES as m (m.mode)}
      <button
        class="modebtn"
        class:on={mcp.mode === m.mode}
        class:full={m.mode === "full"}
        role="radio"
        aria-checked={mcp.mode === m.mode}
        disabled={mcp.busy || !status}
        onclick={() => mcp.setMode(m.mode)}
      >
        <span class="mlabel mono">{m.label}</span>
        <span class="mblurb silk">{m.blurb}</span>
      </button>
    {/each}
  </div>

  {#if mcp.pending.length > 0}
    <div class="pendingbox">
      <span class="silk amber">awaiting your confirmation · {mcp.pending.length}</span>
    </div>
  {/if}

  <div class="feed">
    <span class="silk">agent activity</span>
    {#if mcp.feed.length === 0}
      <div class="empty silk">quiet — no agent calls this session</div>
    {/if}
    {#each mcp.feed as f (f.id)}
      <div class="entry" class:approved={f.resolution === "approved"} class:denied={f.resolution === "denied" || f.resolution === "expired"}>
        <span class="glyph mono">{RES_GLYPH[f.resolution]}</span>
        <div class="etext">
          <span class="etool mono">{f.tool}</span>
          <span class="esum">{f.summary}</span>
        </div>
        <span class="eat mono">{f.at}</span>
      </div>
    {/each}
  </div>

  <div class="docs silk">
    connect: <span class="mono">claude mcp add --transport http aura …</span> — see docs/mcp-usage.md
  </div>
</div>

<style>
  .panel {
    display: flex;
    flex-direction: column;
    gap: 14px;
    flex: 1;
    min-height: 0;
    overflow-y: auto;
    padding-right: 2px;
  }

  .statusbox {
    border: 1px solid var(--glass-border);
    border-radius: 6px;
    background: rgba(10, 13, 23, 0.6);
    padding: 9px 11px;
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .led {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: rgba(96, 130, 190, 0.3);
  }
  .led.on {
    background: #5cf2b8;
    box-shadow: 0 0 8px rgba(92, 242, 184, 0.7);
    animation: led-breathe 2.4s ease-in-out infinite;
  }
  @keyframes led-breathe {
    50% {
      box-shadow: 0 0 3px rgba(92, 242, 184, 0.3);
    }
  }
  .state {
    font-size: 10px;
    letter-spacing: 0.18em;
    color: var(--text);
  }
  .addr {
    font-size: 10px;
    color: var(--text-dim);
    margin-left: auto;
  }
  .token .fp {
    font-size: 11px;
    color: var(--cyan);
  }
  .hint {
    letter-spacing: 0.06em;
    text-transform: none;
  }
  .hint .mono {
    color: var(--text-dim);
    font-size: 9px;
  }

  .modes {
    display: flex;
    flex-direction: column;
    gap: 5px;
  }
  .modebtn {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 2px;
    padding: 7px 10px;
    border-radius: 5px;
    border: 1px solid var(--glass-border);
    background: rgba(16, 20, 42, 0.35);
    cursor: pointer;
    text-align: left;
    transition: border-color 120ms, box-shadow 120ms;
  }
  .modebtn:hover:not(:disabled) {
    border-color: rgba(122, 160, 220, 0.3);
  }
  .modebtn.on {
    border-color: var(--cyan-dim);
    box-shadow: 0 0 10px rgba(82, 229, 255, 0.15);
  }
  .modebtn.on .mlabel {
    color: var(--cyan);
  }
  /* FULL mode is a hazard — say so when armed */
  .modebtn.full.on {
    border-color: rgba(255, 200, 87, 0.5);
    box-shadow: 0 0 10px rgba(255, 200, 87, 0.2);
  }
  .modebtn.full.on .mlabel {
    color: var(--amber);
  }
  .mlabel {
    font-size: 10px;
    letter-spacing: 0.18em;
    color: var(--text);
  }
  .mblurb {
    letter-spacing: 0.08em;
  }

  .pendingbox {
    border: 1px dashed rgba(255, 200, 87, 0.5);
    border-radius: 5px;
    padding: 7px 10px;
    background: rgba(255, 200, 87, 0.05);
  }
  .amber {
    color: var(--amber);
  }

  .feed {
    display: flex;
    flex-direction: column;
    gap: 6px;
  }
  .empty {
    padding: 6px 0;
  }
  .entry {
    display: flex;
    align-items: flex-start;
    gap: 8px;
    padding: 6px 9px;
    border-radius: 5px;
    border: 1px solid rgba(96, 130, 190, 0.12);
    background: rgba(10, 13, 23, 0.5);
  }
  .glyph {
    font-size: 11px;
    color: var(--amber);
    width: 12px;
    flex: none;
  }
  .entry.approved .glyph {
    color: #5cf2b8;
  }
  .entry.denied .glyph {
    color: var(--red);
  }
  .etext {
    flex: 1;
    min-width: 0;
    display: flex;
    flex-direction: column;
    gap: 1px;
  }
  .etool {
    font-size: 9px;
    letter-spacing: 0.12em;
    color: var(--magenta);
  }
  .esum {
    font-size: 10px;
    color: var(--text-dim);
    overflow: hidden;
    text-overflow: ellipsis;
    display: -webkit-box;
    -webkit-line-clamp: 2;
    line-clamp: 2;
    -webkit-box-orient: vertical;
  }
  .eat {
    font-size: 9px;
    color: var(--text-faint);
    flex: none;
  }

  .docs {
    margin-top: auto;
    padding-top: 6px;
    border-top: 1px solid rgba(96, 130, 190, 0.08);
    text-transform: none;
    letter-spacing: 0.06em;
  }
  .docs .mono {
    color: var(--text-dim);
    font-size: 9px;
  }
</style>
