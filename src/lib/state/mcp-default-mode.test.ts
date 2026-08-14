/**
 * MCP default policy mode: the mcpDefaultMode preference is the mode the
 * agent server should wake up in. mcp.init() pushes it to the server when
 * the server reports something else, and later preference changes apply
 * immediately — the preference is the durable intent, the server the
 * per-session reality.
 */

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { McpStatus } from "../types/ipc";

function status(mode: McpStatus["policy"]["mode"]): McpStatus {
  return {
    running: true,
    port: 4111,
    tokenFingerprint: "deadbeef",
    policy: { mode },
    pending: [],
  };
}

const mcpGetStatus = vi.fn(() => Promise.resolve(status("confirmDestructive")));
const mcpSetPolicy = vi.fn((policy: McpStatus["policy"]) => Promise.resolve(policy));

vi.mock("../tauri", () => ({
  backend: {
    on: () => () => {},
    mcpGetStatus: () => mcpGetStatus(),
    mcpSetPolicy: (policy: McpStatus["policy"]) => mcpSetPolicy(policy),
    getTransportState: () => Promise.resolve({}),
    listTracks: () => Promise.resolve([]),
  },
}));

const { prefs } = await import("../prefs/prefs.svelte");
const { mcp } = await import("./mcp.svelte");

beforeEach(() => {
  vi.useFakeTimers(); // keep init()'s poll interval off the real clock
  // Reset the singletons FIRST — restoring the pref may fire the live-apply
  // listener a previous test's init() registered — then silence the mocks.
  prefs.restoreDefaults();
  mcp.status = null;
  vi.clearAllMocks();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("mcpDefaultMode at boot", () => {
  it("pushes the preferred mode when the server reports a different one", async () => {
    prefs.set("mcpDefaultMode", "readOnly");
    await mcp.init();
    expect(mcpSetPolicy).toHaveBeenCalledWith(expect.objectContaining({ mode: "readOnly" }));
    expect(mcp.mode).toBe("readOnly");
  });

  it("leaves the server alone when it already matches the preference", async () => {
    await mcp.init(); // pref default == server's confirmDestructive
    expect(mcpSetPolicy).not.toHaveBeenCalled();
  });
});

describe("mcpDefaultMode changed while running", () => {
  it("applies the new mode immediately", async () => {
    await mcp.init();
    prefs.set("mcpDefaultMode", "full");
    await vi.waitFor(() =>
      expect(mcpSetPolicy).toHaveBeenCalledWith(expect.objectContaining({ mode: "full" })),
    );
  });

  it("does not echo a panel-side mode change back into the preference", async () => {
    await mcp.init();
    await mcp.setMode("full");
    expect(prefs.values.mcpDefaultMode).toBe("confirmDestructive");
  });
});
