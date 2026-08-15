/**
 * Open a plugin surface in the right dock. The dock prefers the Zyn patch
 * browser whenever `zyn.openInstanceId` is set, so "open params" must
 * dismiss that view first — otherwise the piano-roll / track chip looks
 * dead after a patch session.
 */
import { plugins } from "./plugins.svelte";
import { ui } from "./ui.svelte";
import { zyn } from "./zynpatches.svelte";

export function openPluginParams(instanceId: string): Promise<void> {
  zyn.close();
  ui.dock = "plugins";
  return plugins.openParams(instanceId);
}
