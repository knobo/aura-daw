/**
 * MCP server mirror: status/policy, the pending-confirmation queue driving
 * the big confirm dialog, and a local activity feed of agent requests and
 * how they were resolved. The token itself NEVER reaches this layer — only
 * the 8-char fingerprint and the token-file path hint (docs/mcp-usage.md).
 */

import { backend } from "../tauri";
import { project } from "./project.svelte";
import { midi } from "./midi.svelte";
import type { McpPolicyMode, McpStatus, PendingConfirmation } from "../types/ipc";

export interface McpFeedEntry {
  id: string;
  tool: string;
  summary: string;
  at: string; // HH:MM:SS local
  resolution: "requested" | "approved" | "denied" | "expired";
}

const FEED_MAX = 24;

function clock(): string {
  return new Date().toLocaleTimeString([], { hour12: false });
}

class McpStore {
  status = $state<McpStatus | null>(null);
  /** Confirmations awaiting the user, oldest first; [0] is in the dialog. */
  pending = $state<PendingConfirmation[]>([]);
  feed = $state<McpFeedEntry[]>([]);
  busy = $state(false);

  private pollTimer: ReturnType<typeof setInterval> | null = null;
  private unlisten: (() => void) | null = null;
  /** Ids the user already resolved — an in-flight (stale) status poll must
   * not resurrect them as pending. */
  private resolved = new Set<string>();

  get current(): PendingConfirmation | null {
    return this.pending[0] ?? null;
  }
  get mode(): McpPolicyMode {
    return this.status?.policy.mode ?? "confirmDestructive";
  }

  private pushFeed(entry: McpFeedEntry) {
    this.feed = [entry, ...this.feed].slice(0, FEED_MAX);
  }

  private markFeed(id: string, resolution: McpFeedEntry["resolution"]) {
    this.feed = this.feed.map((f) => (f.id === id ? { ...f, resolution, at: clock() } : f));
  }

  async refresh() {
    try {
      const status = await backend.mcpGetStatus();
      this.status = status;
      // Adopt server-side pending we have not seen (e.g. requested pre-listen),
      // and drop entries the server no longer knows (timeout ⇒ deny).
      const server = new Map(status.pending.map((p) => [p.id, p]));
      for (const p of status.pending) {
        if (!this.pending.some((q) => q.id === p.id)) this.accept(p);
      }
      for (const p of this.pending) {
        if (!server.has(p.id)) this.markFeed(p.id, "expired");
      }
      this.pending = this.pending.filter((p) => server.has(p.id));
    } catch (err) {
      console.warn("[aura] mcp_get_status failed:", err);
    }
  }

  private accept(p: PendingConfirmation) {
    if (this.resolved.has(p.id)) return;
    if (this.pending.some((q) => q.id === p.id)) return;
    this.pending = [...this.pending, p];
    this.pushFeed({ id: p.id, tool: p.tool, summary: p.summary, at: clock(), resolution: "requested" });
  }

  async init() {
    await this.refresh();
    this.unlisten?.();
    this.unlisten = backend.on("mcp://confirm-requested", (p) => this.accept(p));
    // Slow poll keeps status honest (timeouts, out-of-band policy edits).
    this.pollTimer ??= setInterval(() => void this.refresh(), 5000);
  }

  async setMode(mode: McpPolicyMode) {
    if (!this.status) return;
    this.busy = true;
    try {
      const policy = await backend.mcpSetPolicy({ ...this.status.policy, mode });
      this.status = { ...this.status, policy };
    } catch (err) {
      console.warn("[aura] mcp_set_policy failed:", err);
    } finally {
      this.busy = false;
    }
  }

  async resolve(confirmationId: string, approve: boolean) {
    this.resolved.add(confirmationId);
    this.pending = this.pending.filter((p) => p.id !== confirmationId);
    this.markFeed(confirmationId, approve ? "approved" : "denied");
    try {
      await backend.mcpConfirmPending(confirmationId, approve);
    } catch (err) {
      console.warn("[aura] mcp_confirm_pending failed:", err);
    }
    void this.refresh();
    if (approve) {
      // the approved tool may have changed anything — re-pull the snapshot
      // (give the parked call a moment to actually run)
      setTimeout(() => {
        void project.reload();
        void midi.refresh();
      }, 400);
    }
  }
}

export const mcp = new McpStore();
