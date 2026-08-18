import { describe, expect, it } from "vitest";
import { DOCK_SHORTCUT, dockTabForKey, type DockTab } from "./ui.svelte";

/**
 * ui.svelte.ts warns in its own comments that a dock key bound on top of an
 * existing bare-key shortcut "would silently shadow a shipped shortcut" —
 * there is no runtime error, the older handler simply never runs again. These
 * tests turn that warning into something that fails in CI instead.
 */

/** Every dock panel, mirroring the DockTab union minus the closed state. */
const TABS: Exclude<DockTab, "">[] = [
  "composer",
  "generate",
  "history",
  "hum",
  "library",
  "instruments",
  "plugins",
  "mcp",
  "midi",
];

/**
 * Bare (unmodified) letters App.svelte's window keydown handler claims for
 * itself, with the branch that owns each. `dockTabForKey` is tested BEFORE
 * "k" and "c" in that chain, so a dock tab bound to one of these would win
 * and silently kill the shipped behaviour. Keep this in step with
 * App.svelte's handler.
 */
const RESERVED_BARE_KEYS: Record<string, string> = {
  s: "view.snap toggle",
  k: "launch.togglePanel",
  c: "library CLIPS root",
};

describe("dock shortcuts", () => {
  it("binds every dock panel", () => {
    for (const tab of TABS) {
      expect(DOCK_SHORTCUT[tab], `${tab} has no key`).toBeTruthy();
    }
    // Nothing bound that is not a real panel.
    expect(Object.keys(DOCK_SHORTCUT).sort()).toEqual([...TABS].sort());
  });

  it("gives each panel a distinct key", () => {
    const keys = TABS.map((t) => DOCK_SHORTCUT[t]);
    expect(new Set(keys).size, `duplicate key in ${keys.join(",")}`).toBe(keys.length);
  });

  it("keeps off App.svelte's other bare-key shortcuts", () => {
    for (const tab of TABS) {
      const key = DOCK_SHORTCUT[tab];
      const owner = RESERVED_BARE_KEYS[key];
      expect(owner, `${tab} is bound to "${key}", which belongs to ${owner}`).toBeUndefined();
    }
  });

  it("resolves a key to its panel, ignoring case", () => {
    expect(dockTabForKey("d")).toBe("midi");
    expect(dockTabForKey("D")).toBe("midi");
    expect(dockTabForKey("m")).toBe("mcp");
  });

  it("resolves nothing for an unbound key", () => {
    expect(dockTabForKey("z")).toBeUndefined();
    expect(dockTabForKey("")).toBeUndefined();
  });
});
